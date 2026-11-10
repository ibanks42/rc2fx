//! The worker's staged preparation after each published frame: when it runs,
//! when it stops, and that it never changes a result.
use super::*;
use std::{
    cell::Cell,
    sync::{Mutex, mpsc},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Process(u64),
    Step,
    Reset,
}
#[derive(Clone, Copy)]
enum Action {
    Push(u64),
    Stop(u8),
    Fail,
}

struct Rig {
    shared: Arc<Shared>,
    input: Mutex<Producer>,
    output: Mutex<Consumer>,
    /// Results taken from the output queue so far (it holds only eight).
    published: Mutex<Vec<Frame>>,
}
impl Rig {
    fn collect(&self) {
        let mut output = self.output.lock().unwrap();
        while let Some(frame) = output.pop() {
            self.published.lock().unwrap().push(frame);
        }
    }
    fn act(&self, action: Action) {
        match action {
            Action::Push(sequence) => {
                assert!(self.input.lock().unwrap().push(&frame(sequence, &[])));
                assert!(self.shared.wake.notify());
            }
            Action::Stop(mode) => {
                self.shared.stop.store(mode, Ordering::Release);
                assert!(self.shared.wake.notify());
            }
            Action::Fail => self.shared.failed.store(true, Ordering::Release),
        }
    }
}

fn frame(sequence: u64, samples: &[f32]) -> Frame {
    let mut frame = Frame {
        sequence,
        valid: true,
        ..Frame::default()
    };
    frame.samples[0] = sequence as f32;
    frame.samples[..samples.len()].copy_from_slice(samples);
    frame
}

fn geometry() -> Geometry {
    Geometry::for_lookahead(0).unwrap()
}

/// Runs the worker loop on its own thread until it returns (or fails the test
/// after five seconds), with `model` built from the rig. Every preparation
/// step is timed, as on a bench run.
fn drive(initial: &[u64], model: impl FnOnce(Arc<Rig>) -> Box<dyn Model>) -> Arc<Rig> {
    drive_timed(initial, true, model)
}

/// `drive` with per-step preparation timing on or off.
fn drive_timed(
    initial: &[u64],
    step_timing: bool,
    model: impl FnOnce(Arc<Rig>) -> Box<dyn Model>,
) -> Arc<Rig> {
    let (mut input, consumer) = queue::channel();
    let (producer, output) = queue::channel();
    for &sequence in initial {
        assert!(input.push(&frame(sequence, &[])));
    }
    let shared = Arc::new(Shared::default());
    shared
        .prep
        .step_timing
        .store(step_timing, Ordering::Relaxed);
    let rig = Arc::new(Rig {
        shared,
        input: Mutex::new(input),
        output: Mutex::new(output),
        published: Mutex::new(Vec::new()),
    });
    let model = model(rig.clone());
    let shared = rig.shared.clone();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        run(model, consumer, producer, &shared, geometry());
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("worker loop returned");
    rig.collect();
    rig
}

/// Five preparation steps per frame, as DeepFilterNet3's five GRU layers;
/// acts on the rig after given processed frames and global step counts.
struct Scripted {
    rig: Arc<Rig>,
    log: Arc<Mutex<Vec<Event>>>,
    pending: usize,
    steps: usize,
    on_process: Vec<(u64, Action)>,
    on_step: Vec<(usize, Action)>,
}
const LAYERS: usize = 5;
impl Model for Scripted {
    fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
        let sequence = samples[0] as u64;
        self.log.lock().unwrap().push(Event::Process(sequence));
        self.pending = LAYERS;
        for &(at, action) in &self.on_process {
            if at == sequence {
                self.rig.act(action);
            }
        }
        Ok(0.5)
    }
    fn reset(&mut self) -> Result<(), Error> {
        self.log.lock().unwrap().push(Event::Reset);
        self.pending = LAYERS;
        Ok(())
    }
    fn prepare_step(&mut self) -> bool {
        if self.pending == 0 {
            return false;
        }
        self.pending -= 1;
        self.steps += 1;
        self.log.lock().unwrap().push(Event::Step);
        for &(at, action) in &self.on_step {
            if at == self.steps {
                self.rig.act(action);
            }
        }
        true
    }
    fn prep_pending(&self) -> usize {
        self.pending
    }
}

fn scripted(
    initial: &[u64],
    on_process: &[(u64, Action)],
    on_step: &[(usize, Action)],
) -> (Vec<Event>, Preparation) {
    let (events, prep, _) = scripted_timed(initial, true, on_process, on_step);
    (events, prep)
}

fn scripted_timed(
    initial: &[u64],
    step_timing: bool,
    on_process: &[(u64, Action)],
    on_step: &[(usize, Action)],
) -> (Vec<Event>, Preparation, Snapshot) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let rig = drive_timed(initial, step_timing, |rig| {
        Box::new(Scripted {
            rig,
            log: log.clone(),
            pending: 0,
            steps: 0,
            on_process: on_process.to_vec(),
            on_step: on_step.to_vec(),
        })
    });
    let events = log.lock().unwrap().clone();
    let telemetry = Telemetry(rig.shared.clone());
    (events, telemetry.preparation(), telemetry.snapshot())
}

fn steps(n: usize) -> impl Iterator<Item = Event> {
    std::iter::repeat_n(Event::Step, n)
}

#[test]
fn an_idle_worker_prepares_every_step_after_publishing() {
    let (events, prep) = scripted(&[0], &[], &[(LAYERS, Action::Stop(2))]);
    let expected: Vec<_> = [Event::Process(0)]
        .into_iter()
        .chain(steps(LAYERS))
        .collect();
    assert_eq!(events, expected);
    assert!(prep.enabled);
    assert_eq!(
        (prep.steps, prep.skipped, prep.abandoned),
        (LAYERS as u64, 0, 0)
    );
    assert_eq!(prep.pass_wall.calls + prep.pass_wall.clock_errors, 1);
    assert!(prep.step_timing);
    assert_eq!(
        prep.step_wall.calls + prep.step_wall.clock_errors,
        LAYERS as u64
    );
}

/// A worker without per-step timing: the steps, the pass timings and the CPU
/// total are recorded exactly as with it; only `step_wall` stays empty, and
/// the report says it was not measured.
#[test]
fn without_step_timing_the_pass_is_still_measured() {
    for step_timing in [false, true] {
        let (events, prep, snapshot) = scripted_timed(
            &[0],
            step_timing,
            &[],
            &[(2, Action::Push(1)), (2 + LAYERS, Action::Stop(2))],
        );
        let expected: Vec<_> = [Event::Process(0)]
            .into_iter()
            .chain(steps(2))
            .chain([Event::Process(1)])
            .chain(steps(LAYERS))
            .collect();
        assert_eq!(events, expected);
        assert_eq!(prep.step_timing, step_timing);
        assert_eq!(
            (prep.steps, prep.skipped, prep.abandoned),
            (2 + LAYERS as u64, 0, 1)
        );
        assert_eq!(prep.pass_wall.calls + prep.pass_wall.clock_errors, 2);
        assert_eq!(prep.pass_cpu.calls + prep.pass_cpu.clock_errors, 2);
        assert_eq!(
            prep.step_wall.calls + prep.step_wall.clock_errors,
            if step_timing { 2 + LAYERS as u64 } else { 0 }
        );
        assert_eq!(
            prep.total_cpu_ns,
            snapshot.cpu.total_ns + prep.pass_cpu.total_ns
        );
        // The pass spans its steps: the scripted model's steps take time
        // only through the log's lock, so compare with the step total.
        if step_timing && prep.pass_wall.clock_errors == 0 {
            assert!(prep.pass_wall.total_ns >= prep.step_wall.total_ns);
        }
    }
}

/// A worker takes the process-wide setting when it starts.
#[test]
fn workers_start_with_the_process_wide_step_timing_setting() {
    // It is on until a host turns it off.
    let worker = |expected: bool| {
        let mut worker = Worker::new(0, WorkerOptions::default()).unwrap();
        assert_eq!(worker.telemetry().preparation().step_timing, expected);
        worker
            .finish(StopMode::Cancel, Duration::from_secs(5))
            .unwrap();
    };
    worker(true);
    // Other tests start workers concurrently; they do not read `step_wall`,
    // so a setting that is briefly off does not disturb them. The loop ends
    // with the default restored.
    for on in [false, true] {
        crate::set_preparation_step_timing(on);
        let mut worker = Worker::new(0, WorkerOptions::default()).unwrap();
        assert_eq!(worker.telemetry().preparation().step_timing, on);
        worker
            .finish(StopMode::Cancel, Duration::from_secs(5))
            .unwrap();
    }
}

/// Three preparation steps of at least two milliseconds each after every
/// frame; the last one asks the worker to drain and stop.
struct Slow {
    rig: Arc<Rig>,
    pending: usize,
}
impl Model for Slow {
    fn process(&mut self, _: &mut [f32]) -> Result<f32, Error> {
        self.pending = 3;
        Ok(0.5)
    }
    fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn prepare_step(&mut self) -> bool {
        if self.pending == 0 {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
        self.pending -= 1;
        if self.pending == 0 {
            self.rig.act(Action::Stop(2));
        }
        true
    }
    fn prep_pending(&self) -> usize {
        self.pending
    }
}

/// The pass's wall time covers all of its steps with step timing off too.
/// `pass_wall` is then the only bound on how long a step made arriving input
/// wait.
#[test]
fn the_pass_wall_time_spans_its_steps_with_or_without_step_timing() {
    for step_timing in [false, true] {
        let rig = drive_timed(&[0], step_timing, |rig| Box::new(Slow { rig, pending: 0 }));
        let prep = Telemetry(rig.shared.clone()).preparation();
        assert_eq!((prep.steps, prep.skipped, prep.abandoned), (3, 0, 0));
        assert_eq!(prep.pass_wall.calls + prep.pass_wall.clock_errors, 1);
        if prep.pass_wall.clock_errors == 0 {
            assert!(prep.pass_wall.total_ns >= 6_000_000, "{prep:?}");
            assert_eq!(prep.pass_wall.max_ns, prep.pass_wall.total_ns);
        }
        assert_eq!(
            prep.step_wall.calls + prep.step_wall.clock_errors,
            if step_timing { 3 } else { 0 }
        );
        if step_timing && prep.step_wall.clock_errors == 0 {
            assert!(prep.step_wall.total_ns >= 6_000_000, "{prep:?}");
        }
    }
}

#[test]
fn queued_input_at_publication_prevents_preparation() {
    let (events, prep) = scripted(&[0, 1], &[], &[(LAYERS, Action::Stop(2))]);
    let expected: Vec<_> = [Event::Process(0), Event::Process(1)]
        .into_iter()
        .chain(steps(LAYERS))
        .collect();
    assert_eq!(events, expected);
    assert_eq!((prep.skipped, prep.abandoned), (1, 0));
}

/// Input arriving during a step waits for that step, then preparation stops
/// until the next publication.
#[test]
fn input_arriving_between_steps_stops_preparation() {
    let (events, prep) = scripted(
        &[0],
        &[],
        &[(2, Action::Push(1)), (2 + LAYERS, Action::Stop(2))],
    );
    let expected: Vec<_> = [Event::Process(0)]
        .into_iter()
        .chain(steps(2))
        .chain([Event::Process(1)])
        .chain(steps(LAYERS))
        .collect();
    assert_eq!(events, expected);
    assert_eq!(
        (prep.steps, prep.skipped, prep.abandoned),
        (2 + LAYERS as u64, 0, 1)
    );
}

#[test]
fn a_stop_or_failure_ends_preparation_after_the_current_step() {
    for action in [Action::Stop(1), Action::Stop(2), Action::Fail] {
        let (events, prep) = scripted(&[0], &[], &[(2, action)]);
        let expected: Vec<_> = [Event::Process(0)].into_iter().chain(steps(2)).collect();
        assert_eq!(events, expected);
        assert_eq!((prep.steps, prep.abandoned), (2, 1));
    }
    // Already stopping when the frame is published: nothing runs.
    let (events, prep) = scripted(&[0], &[(0, Action::Stop(2))], &[]);
    assert_eq!(events, [Event::Process(0)]);
    assert_eq!((prep.steps, prep.skipped), (0, 1));
}

/// A sequence gap resets the model (discarding what it prepared) before the
/// next frame; preparation resumes after that frame is published.
#[test]
fn a_sequence_gap_resets_before_the_next_frame() {
    let (events, _) = scripted(
        &[0],
        &[],
        &[(3, Action::Push(2)), (3 + LAYERS, Action::Stop(2))],
    );
    let expected: Vec<_> = [Event::Process(0)]
        .into_iter()
        .chain(steps(3))
        .chain([Event::Reset, Event::Process(2)])
        .chain(steps(LAYERS))
        .collect();
    assert_eq!(events, expected);
}

/// Without the flag the worker never asks the model to prepare (the
/// scripted model asserts so) and the counters stay zero; with it, an idle
/// worker prepares everything before the stop arrives.
#[test]
fn preparation_runs_only_with_the_flag() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let rig = drive(&[0], |rig| {
        let stopper = rig.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            stopper.act(Action::Stop(2));
        });
        Box::new(Scripted {
            rig,
            log: log.clone(),
            pending: 0,
            steps: 0,
            on_process: Vec::new(),
            on_step: Vec::new(),
        })
    });
    let events = log.lock().unwrap().clone();
    let prep = Telemetry(rig.shared.clone()).preparation();
    assert!(prep.enabled);
    let expected: Vec<_> = [Event::Process(0)]
        .into_iter()
        .chain(steps(LAYERS))
        .collect();
    assert_eq!(events, expected);
    assert_eq!(prep.steps, LAYERS as u64);
    assert_eq!(
        prep.total_cpu_ns,
        rig.shared.cpu.snapshot().total_ns + prep.pass_cpu.total_ns
    );
}

/// DeepFilterNet3 behind the worker loop, with the next frame arriving at
/// every point of the preparation (queued at publication, after each
/// number of steps, after all of them) and a sequence gap: every result is
/// bit for bit what the model gives processing the same frames in order.
#[test]
fn deep_filter_results_match_the_unprepared_model() {
    struct Feed {
        model: VendorModel,
        rig: Arc<Rig>,
        frames: Vec<(u64, Vec<f32>)>,
        /// Per frame: push the next one during `process` (None) or after
        /// this many preparation steps (or all of them, if fewer).
        after: Vec<Option<usize>>,
        processed: usize,
        steps: usize,
        pushed: Cell<bool>,
    }
    impl Feed {
        fn push_after(&self, i: usize) {
            self.pushed.set(true);
            match self.frames.get(i + 1) {
                Some((sequence, samples)) => {
                    assert!(
                        self.rig
                            .input
                            .lock()
                            .unwrap()
                            .push(&frame(*sequence, samples))
                    );
                    assert!(self.rig.shared.wake.notify());
                }
                None => self.rig.act(Action::Stop(2)),
            }
        }
    }
    impl Model for Feed {
        fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
            let i = self.processed;
            self.processed += 1;
            self.rig.collect();
            let voice = self.model.process(samples);
            self.steps = 0;
            self.pushed.set(false);
            if self.after[i].is_none() {
                self.push_after(i);
            }
            voice
        }
        fn reset(&mut self) -> Result<(), Error> {
            self.model.reset()
        }
        fn prepare_step(&mut self) -> bool {
            self.steps += 1;
            self.model.prepare_step()
        }
        /// The worker asks before and after each step, so a push here lands
        /// between steps.
        fn prep_pending(&self) -> usize {
            let pending = self.model.prep_pending();
            let i = self.processed - 1;
            if !self.pushed.get()
                && let Some(k) = self.after[i]
                && (self.steps >= k || pending == 0)
            {
                self.push_after(i);
            }
            pending
        }
    }

    let g = geometry();
    let mut seed = 0x2545_f491u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1 << 23) as f32 - 1.
    };
    let sequences: Vec<u64> = (0..48).filter(|&s| s != 21 && s != 22).collect();
    let frames: Vec<(u64, Vec<f32>)> = sequences
        .iter()
        .map(|&s| {
            let samples = (0..g.frame_samples)
                .map(|i| {
                    let t = (s as usize * g.frame_samples + i) as f32 / 48_000.;
                    0.2 * (t * 180. * std::f32::consts::TAU).sin() + 0.02 * noise()
                })
                .collect();
            (s, samples)
        })
        .collect();
    let after: Vec<Option<usize>> = (0..frames.len())
        .map(|i| {
            if i % 7 == 3 {
                None
            } else {
                Some(i % (LAYERS + 2))
            }
        })
        .collect();

    let mut reference = VendorModel::new(0).unwrap();
    let mut expected = Vec::new();
    let mut previous = None;
    for (sequence, samples) in &frames {
        if previous.is_some_and(|p: u64| p + 1 != *sequence) {
            reference.reset().unwrap();
        }
        previous = Some(*sequence);
        let mut f = frame(*sequence, samples);
        let voice = reference
            .process(&mut f.samples[..g.frame_samples])
            .unwrap();
        expected.push((f, voice));
    }

    let first = frames[0].clone();
    let rig = drive(&[], |rig| {
        assert!(rig.input.lock().unwrap().push(&frame(first.0, &first.1)));
        let model = VendorModel::new(0).unwrap();
        Box::new(Feed {
            model,
            rig,
            frames: frames.clone(),
            after: after.clone(),
            processed: 0,
            steps: 0,
            pushed: Cell::new(false),
        })
    });
    let published = rig.published.lock().unwrap().clone();
    assert_eq!(published.len(), expected.len());
    assert_eq!(rig.shared.output_drops.load(Ordering::Relaxed), 0);
    for ((f, voice), actual) in expected.iter().zip(&published) {
        assert_eq!(actual.sequence, f.sequence);
        assert_eq!(
            actual.voice.to_bits(),
            voice.to_bits(),
            "frame {}",
            f.sequence
        );
        let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(
            bits(&actual.samples[..g.frame_samples]),
            bits(&f.samples[..g.frame_samples]),
            "frame {}",
            f.sequence
        );
    }
    let prep = Telemetry(rig.shared.clone()).preparation();
    assert!(
        prep.steps > 0 && prep.skipped > 0 && prep.abandoned > 0,
        "{prep:?}"
    );
    assert!(prep.step_wall.max_ns > 0 || prep.step_wall.clock_errors > 0);
}
