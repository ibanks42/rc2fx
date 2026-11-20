//! Click-freedom tests for live highpass, compressor, limiter, and order edits.
//!
//! Asserts that no sample-to-sample step after a live change exceeds the
//! settled max step (plus a small margin, or a floor for DC input) and that
//! the tail converges to a processor initialized with the final settings.
use super::super::*;

/// The mixer's period: settings change between blocks of this length.
const PERIOD: usize = 112;
/// Long enough after a change for every fade and smoother to finish.
const SETTLE: usize = 24_000;
/// The settled stretch measured before each change and at the end.
const SETTLED: usize = 4_800;

fn sine(amplitude: f64, offset: f64) -> impl Fn(usize) -> f32 {
    move |n| {
        (offset + amplitude * (n as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin()) as f32
    }
}

/// `input` through a mono output, `settings(block)` prepared for each block
/// of `PERIOD` samples (prepared again only when it changes, as the mixer
/// does).
fn render(
    len: usize,
    input: &dyn Fn(usize) -> f32,
    settings: &dyn Fn(usize) -> Target,
) -> Vec<f32> {
    let mut target = settings(0);
    let mut p = Prepared::new(&target, 48_000).unwrap();
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    let mut output = vec![0.0; len];
    for (block, start) in (0..len).step_by(PERIOD).enumerate() {
        let next = settings(block);
        if next != target {
            target = next;
            p = Prepared::new(&target, 48_000).unwrap();
        }
        let end = (start + PERIOD).min(len);
        let x: Vec<f32> = (start..end).map(input).collect();
        dsp.process(&p, [&x, &[]], [&mut output[start..end], &mut []])
            .unwrap();
    }
    output
}

fn largest_step(y: &[f32]) -> f64 {
    y.windows(2)
        .map(|w| (f64::from(w[1]) - f64::from(w[0])).abs())
        .fold(0.0, f64::max)
}

/// Runs `base` with `changes` applied one after another, `SETTLE` samples
/// apart from sample `SETTLE` on, and checks the output as the module
/// describes, with a margin of 25 %.
fn assert_click_free(
    name: &str,
    input: &dyn Fn(usize) -> f32,
    base: Target,
    changes: &[&dyn Fn(&mut Target)],
    floor: f64,
) {
    assert_click_free_within(name, input, base, changes, 1.25, floor);
}

/// `assert_click_free` with the margin `ratio`.
fn assert_click_free_within(
    name: &str,
    input: &dyn Fn(usize) -> f32,
    base: Target,
    changes: &[&dyn Fn(&mut Target)],
    ratio: f64,
    floor: f64,
) {
    let switches: Vec<usize> = (1..=changes.len()).map(|k| k * SETTLE).collect();
    let len = (changes.len() + 1) * SETTLE;
    let settings = |block: usize| {
        let mut t = base.clone();
        for (k, change) in changes.iter().enumerate() {
            if block * PERIOD >= switches[k] {
                change(&mut t);
            }
        }
        t
    };
    let y = render(len, input, &settings);
    let settled = switches
        .iter()
        .chain([&len])
        .map(|&end| largest_step(&y[end - SETTLED..end]))
        .fold(0.0, f64::max);
    let bound = ratio * settled + floor;
    let first = switches[0] - SETTLED;
    let (at, step) = y[first..]
        .windows(2)
        .map(|w| (f64::from(w[1]) - f64::from(w[0])).abs())
        .enumerate()
        .fold(
            (0, 0.0),
            |most, (at, step)| if step > most.1 { (at, step) } else { most },
        );
    assert!(
        step <= bound,
        "{name}: a step of {step} at sample {} is more than {bound} (settled {settled})",
        first + at + 1
    );
    // Where the final settings, held from the start, end.
    let last = settings(len / PERIOD);
    let reference = render(len, input, &|_| last.clone());
    let error = y[len - SETTLED..]
        .iter()
        .zip(&reference[len - SETTLED..])
        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs())
        .fold(0.0, f64::max);
    assert!(
        error < 1e-4,
        "{name}: ends {error} away from the new settings"
    );
}

fn output() -> Target {
    Target::new("out", "Out", TargetKind::Output, 1)
}

#[test]
fn the_highpass_switched_off_and_on_does_not_click() {
    let mut base = output();
    base.highpass.enabled = true;
    base.highpass.frequency = 80.0;
    let off: &dyn Fn(&mut Target) = &|t| t.highpass.enabled = false;
    let on: &dyn Fn(&mut Target) = &|t| t.highpass.enabled = true;
    // A constant is what a highpass takes away: switched at once, it is a
    // step of the whole constant.
    for (name, input, floor) in [
        (
            "highpass, tone and offset",
            &sine(0.3, 0.3) as &dyn Fn(usize) -> f32,
            1e-4,
        ),
        ("highpass, constant", &sine(0.0, 0.3), 2e-3),
    ] {
        assert_click_free(name, input, base.clone(), &[off, on, off, on], floor);
        // Off at the start, so the first switch on finds the filter's
        // history from the very start of the stream.
        let mut cold = base.clone();
        cold.highpass.enabled = false;
        assert_click_free(name, input, cold, &[on, off, on], floor);
    }
}

#[test]
fn the_highpass_switched_back_on_mid_fade_does_not_click() {
    let mut base = output();
    base.highpass.enabled = true;
    // Off and on again 3 blocks later, while it is still fading out.
    let settings = |block: usize| {
        let mut t = base.clone();
        t.highpass.enabled = !(200..203).contains(&block);
        t
    };
    let input = sine(0.3, 0.3);
    let y = render(SETTLE * 2, &input, &settings);
    let settled = largest_step(&y[200 * PERIOD - SETTLED..200 * PERIOD]);
    let step = largest_step(&y[200 * PERIOD - 1..]);
    assert!(
        step <= 1.25 * settled + 1e-4,
        "step {step}, settled {settled}"
    );
}

fn compressor() -> Target {
    let mut t = output();
    t.compressor.enabled = true;
    t.compressor.threshold = -40.0;
    t.compressor.ratio = 4.0;
    t.compressor.makeup = 0.0;
    t
}

#[test]
fn compressor_setting_jumps_do_not_click() {
    let threshold: [&dyn Fn(&mut Target); 2] = [&|t| t.compressor.threshold = -10.0, &|t| {
        t.compressor.threshold = -25.0
    }];
    let ratio: [&dyn Fn(&mut Target); 2] = [&|t| t.compressor.ratio = 20.0, &|t| {
        t.compressor.ratio = 4.0
    }];
    let makeup: [&dyn Fn(&mut Target); 2] = [&|t| t.compressor.makeup = 12.0, &|t| {
        t.compressor.makeup = 6.0
    }];
    let mut unity = compressor();
    unity.compressor.ratio = 1.0;
    unity.compressor.threshold = -30.0;
    let mut gentle = compressor();
    // Above the tone, so the makeup alone moves the level.
    gentle.compressor.threshold = 0.0;
    for (name, base, changes) in [
        ("compressor threshold", compressor(), threshold),
        ("compressor ratio", unity, ratio),
        ("compressor makeup", gentle, makeup),
    ] {
        assert_click_free(name, &sine(0.2, 0.0), base.clone(), &changes, 1e-4);
        assert_click_free(name, &sine(0.0, 0.2), base, &changes, 2e-3);
    }
}

#[test]
fn the_compressor_switched_off_and_on_does_not_click() {
    let off: &dyn Fn(&mut Target) = &|t| t.compressor.enabled = false;
    let on: &dyn Fn(&mut Target) = &|t| t.compressor.enabled = true;
    let name = "compressor switch";
    assert_click_free(name, &sine(0.2, 0.0), compressor(), &[off, on], 1e-4);
    assert_click_free(name, &sine(0.0, 0.2), compressor(), &[off, on], 2e-3);
}

fn limiter() -> Target {
    let mut t = output();
    t.limiter.enabled = true;
    t.limiter.ceiling = 0.0;
    t.limiter.release = 100.0;
    t
}

#[test]
fn limiter_setting_jumps_do_not_click() {
    let ceiling: [&dyn Fn(&mut Target); 2] = [&|t| t.limiter.ceiling = -12.0, &|t| {
        t.limiter.ceiling = -9.0
    }];
    let release: [&dyn Fn(&mut Target); 2] = [&|t| t.limiter.release = 10.0, &|t| {
        t.limiter.release = 2000.0
    }];
    let mut limiting = limiter();
    limiting.limiter.ceiling = -12.0;
    for (name, base, changes) in [
        ("limiter ceiling", limiter(), ceiling),
        ("limiter release", limiting, release),
    ] {
        assert_click_free(name, &sine(0.5, 0.0), base.clone(), &changes, 1e-4);
        assert_click_free(name, &sine(0.0, 0.5), base, &changes, 2e-3);
    }
}

#[test]
fn the_limiter_switched_off_and_on_does_not_click() {
    let mut base = limiter();
    base.limiter.ceiling = -12.0;
    let off: &dyn Fn(&mut Target) = &|t| t.limiter.enabled = false;
    let on: &dyn Fn(&mut Target) = &|t| t.limiter.enabled = true;
    let name = "limiter switch";
    assert_click_free(name, &sine(0.5, 0.0), base.clone(), &[off, on], 1e-4);
    assert_click_free(name, &sine(0.0, 0.5), base, &[off, on], 2e-3);
}

#[test]
fn an_effect_order_change_does_not_click() {
    let mut base = output();
    base.highpass.enabled = true;
    base.highpass.frequency = 120.0;
    base.eq.enabled = true;
    let mut shelf = Band::new("shelf");
    shelf.band_type = BandType::LowShelf;
    shelf.frequency = 100.0;
    shelf.gain = 9.0;
    base.eq.bands = vec![shelf];
    base.compressor.enabled = true;
    base.compressor.threshold = -30.0;
    base.compressor.ratio = 4.0;
    base.gain.enabled = true;
    base.gain.db = 9.0;
    base.limiter.enabled = true;
    base.limiter.ceiling = -6.0;
    let reversed: &dyn Fn(&mut Target) = &|t| t.order.reverse();
    let restored: &dyn Fn(&mut Target) = &|t| t.order = Effect::ALL.to_vec();
    // The new order starts from the old one's histories, which belong to
    // other signals: what they ring out is faded in with the new order, so
    // the margin is wider than for a stage's switch. A cut from one order to
    // the other steps by twenty times the settled step and more.
    let name = "effect order";
    let changes = [reversed, restored];
    assert_click_free_within(name, &sine(0.2, 0.0), base.clone(), &changes, 2.0, 1e-4);
    assert_click_free_within(name, &sine(0.0, 0.2), base, &changes, 2.0, 2e-2);
}

/// A stage that is off and has faded out costs nothing: the chain is the
/// identity again, and the settings do not move.
#[test]
fn faded_out_stages_leave_the_identity_path() {
    let mut t = output();
    t.highpass.enabled = true;
    t.compressor.enabled = true;
    t.limiter.enabled = true;
    let p = Prepared::new(&t, 48_000).unwrap();
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    t.highpass.enabled = false;
    t.compressor.enabled = false;
    t.limiter.enabled = false;
    t.compressor.threshold = -50.0;
    let off = Prepared::new(&t, 48_000).unwrap();
    let mut y = [0.0; PERIOD];
    let x = [0.25; PERIOD];
    for _ in 0..STAGE_FADE_SAMPLES as usize / PERIOD {
        dsp.process(&off, [&x, &[]], [&mut y, &mut []]).unwrap();
        assert!(!dsp.steady_identity(&off));
    }
    dsp.process(&off, [&x, &[]], [&mut y, &mut []]).unwrap();
    assert!(dsp.steady_identity(&off));
    let before = dsp.stages.comp_threshold_db;
    dsp.process(&off, [&x, &[]], [&mut y, &mut []]).unwrap();
    assert_eq!(y, x);
    assert_eq!(dsp.stages.comp_threshold_db.to_bits(), before.to_bits());
}

/// A setting smoothed to a new value ends on it exactly, so the compressor
/// goes back to looking at its level only above the threshold.
#[test]
fn smoothed_settings_end_on_the_prepared_values() {
    let mut t = compressor();
    t.limiter.enabled = true;
    let p = Prepared::new(&t, 48_000).unwrap();
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    t.compressor.threshold = -17.3;
    t.compressor.ratio = 7.7;
    t.compressor.makeup = 3.1;
    t.limiter.ceiling = -4.4;
    t.limiter.release = 1234.0;
    let p = Prepared::new(&t, 48_000).unwrap();
    let x = [0.1; PERIOD];
    let mut y = [0.0; PERIOD];
    for _ in 0..1000 {
        dsp.process(&p, [&x, &[]], [&mut y, &mut []]).unwrap();
    }
    assert_eq!(
        [
            dsp.stages.comp_threshold_db,
            dsp.stages.comp_slope,
            dsp.stages.comp_makeup,
            dsp.stages.limiter_ceiling,
            dsp.stages.limiter_release
        ]
        .map(f64::to_bits),
        [
            p.threshold_db,
            p.comp_slope,
            p.makeup,
            p.ceiling,
            p.limiter_release
        ]
        .map(f64::to_bits)
    );
}

/// `input` through `dsp` with `p`, a period at a time from sample `from`,
/// calling `before(block)` ahead of each period.
fn drive(
    dsp: &mut Dsp,
    p: &Prepared,
    input: &dyn Fn(usize) -> f32,
    from: usize,
    len: usize,
    mut before: impl FnMut(&mut Dsp, usize),
) -> Vec<f32> {
    let mut output = vec![0.0; len];
    for (block, start) in (0..len).step_by(PERIOD).enumerate() {
        before(dsp, block);
        let end = (start + PERIOD).min(len);
        let x: Vec<f32> = (from + start..from + end).map(input).collect();
        dsp.process(p, [&x, &[]], [&mut output[start..end], &mut []])
            .unwrap();
    }
    output
}

fn quieter() -> (Target, Prepared) {
    let mut t = output();
    t.gain.enabled = true;
    t.gain.db = -12.0;
    let p = Prepared::new(&t, 48_000).unwrap();
    (t, p)
}

/// A processor that replaces another starts silent, stays silent for its
/// delay (its delay line holds nothing yet), then fades in without a step,
/// and is then exactly the processor that started normally.
#[test]
fn a_processor_started_silent_fades_in_after_its_delay() {
    let (_, p) = quieter();
    let input = sine(0.5, 0.0);
    let hold = 300;
    let fade = STAGE_FADE_SAMPLES as usize;
    let mut started = Dsp::without_suppression(&p).unwrap();
    started.start_silent(hold);
    let y = drive(&mut started, &p, &input, 0, SETTLE, |_, _| {});
    let mut normal = Dsp::without_suppression(&p).unwrap();
    let reference = drive(&mut normal, &p, &input, 0, SETTLE, |_, _| {});
    assert!(y[..hold].iter().all(|v| *v == 0.0));
    assert!(y[hold] != 0.0 && y[hold].abs() < 1e-3);
    let settled = largest_step(&reference[SETTLE - SETTLED..]);
    let step = largest_step(&y);
    assert!(
        step <= 1.25 * settled + 1e-4,
        "step {step}, settled {settled}"
    );
    assert_eq!(
        y[hold + fade..]
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        reference[hold + fade..]
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>()
    );
}

/// Handing off fades the output to silence, or to the dry signal, without
/// a step; called back, it fades in again.
#[test]
fn handing_off_fades_to_silence_or_to_the_dry_signal_and_back() {
    let (_, p) = quieter();
    let input = sine(0.5, 0.0);
    let fade = STAGE_FADE_SAMPLES as usize;
    for to in [HandOff::Silence, HandOff::Dry] {
        let mut dsp = Dsp::without_suppression(&p).unwrap();
        let warm = drive(&mut dsp, &p, &input, 0, SETTLE, |_, _| {});
        let mut normal = Dsp::without_suppression(&p).unwrap();
        drive(&mut normal, &p, &input, 0, SETTLE, |_, _| {});
        let reference = drive(&mut normal, &p, &input, SETTLE, SETTLE, |_, _| {});
        let settled = largest_step(&warm[SETTLE - SETTLED..])
            .max(largest_step(&(0..SETTLED).map(&input).collect::<Vec<_>>()));
        // Out for 20 periods, then back.
        let y = drive(&mut dsp, &p, &input, SETTLE, SETTLE, |dsp, block| {
            if block < 20 {
                dsp.hand_off(Some(to));
                if block * PERIOD >= fade {
                    assert!(dsp.handed_off(), "{to:?}");
                }
            } else {
                dsp.hand_off(None);
                assert!(!dsp.handed_off());
            }
        });
        let mut joined = warm[SETTLE - SETTLED..].to_vec();
        joined.extend_from_slice(&y);
        let step = largest_step(&joined);
        assert!(
            step <= 1.25 * settled + 1e-4,
            "{to:?}: step {step}, settled {settled}"
        );
        for (n, v) in y.iter().enumerate().take(20 * PERIOD).skip(fade) {
            match to {
                HandOff::Silence => assert_eq!(*v, 0.0, "sample {n}"),
                HandOff::Dry => assert_eq!(v.to_bits(), input(SETTLE + n).to_bits(), "{n}"),
            }
        }
        // Back in, it is the processor that never handed off.
        assert_eq!(&y[SETTLE - SETTLED..], &reference[SETTLE - SETTLED..]);
    }
}

/// A processor without a delay that takes over from the unprocessed path
/// starts at the dry signal and fades to its output without a step.
#[test]
fn a_processor_started_dry_fades_from_the_dry_signal() {
    let (_, p) = quieter();
    let input = sine(0.5, 0.0);
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    dsp.start_dry();
    let y = drive(&mut dsp, &p, &input, 0, SETTLE, |_, _| {});
    assert!((y[0] - input(0)).abs() < 1e-6 && (y[1] - input(1)).abs() < 1e-3);
    let settled = largest_step(&(0..SETTLED).map(&input).collect::<Vec<_>>());
    let step = largest_step(&y);
    assert!(
        step <= 1.25 * settled + 1e-4,
        "step {step}, settled {settled}"
    );
    let mut normal = Dsp::without_suppression(&p).unwrap();
    let reference = drive(&mut normal, &p, &input, 0, SETTLE, |_, _| {});
    assert_eq!(&y[SETTLE - SETTLED..], &reference[SETTLE - SETTLED..]);
}
