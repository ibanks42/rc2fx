//! The inline endpoint: the model runs on the calling thread as each frame
//! completes, so the output needs no worker lead. For a model whose worst
//! frame fits the caller's period budget this is the lowest-latency
//! arrangement: frame assembly plus the model's own delay, nothing for
//! scheduling.
//!
//! The calling thread is the mixer's DSP helper, which has a whole device
//! period on a core of its own. The first call warms the model on that
//! thread, the only allocation it ever makes.
use crate::{
    Error, Geometry, Model, Shared, Snapshot, StopMode, Telemetry, Terminal, VendorModel, metrics,
    queue::MAX_FRAME,
};
use rc2fx_dsp::MAX_SUPPRESSION_DELAY;
use std::sync::{Arc, atomic::Ordering};

pub struct Inline {
    geometry: Geometry,
    shared: Arc<Shared>,
    model: Box<dyn Model>,
    assembling: [f32; MAX_FRAME],
    /// The last result: output for the frame's samples after the one it was
    /// computed on.
    playing: [f32; MAX_FRAME],
    playing_valid: bool,
    score: f32,
    phase: usize,
    frames: u64,
    samples: u64,
    dry: [f32; MAX_SUPPRESSION_DELAY],
    dry_position: usize,
    voice: Option<f32>,
    warmed: bool,
    finished: bool,
}

impl Inline {
    /// The inline endpoint has no scheduling allowance; it keeps the model's
    /// frame and intrinsic delay and removes only the worker lead.
    pub fn new(lookahead: u8) -> Result<Self, Error> {
        let mut geometry = Geometry::for_lookahead(lookahead)?;
        geometry.worker_lead_samples = 0;
        let model = VendorModel::new(lookahead)?;
        Self::with_model(geometry, Box::new(model))
    }
    pub(crate) fn with_model(geometry: Geometry, model: Box<dyn Model>) -> Result<Self, Error> {
        if geometry.worker_lead_samples != 0 {
            return Err(Error::InvalidAllowance);
        }
        if geometry.frame_samples == 0
            || geometry.frame_samples > MAX_FRAME
            || geometry.latency_samples() > MAX_SUPPRESSION_DELAY
        {
            return Err(Error::ModelGeometry);
        }
        let shared = Arc::new(Shared::default());
        // No thread: ownership never needs joining.
        shared.termination.store(3, Ordering::Release);
        Ok(Self {
            geometry,
            shared,
            model,
            assembling: [0.; MAX_FRAME],
            playing: [0.; MAX_FRAME],
            playing_valid: false,
            score: 0.,
            phase: 0,
            frames: 0,
            samples: 0,
            dry: [0.; MAX_SUPPRESSION_DELAY],
            dry_position: 0,
            voice: None,
            warmed: false,
            finished: false,
        })
    }
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }
    pub fn latency_samples(&self) -> usize {
        self.geometry.latency_samples()
    }
    pub fn telemetry(&self) -> Telemetry {
        Telemetry(self.shared.clone())
    }
    pub fn voice(&self) -> Option<f32> {
        self.voice
    }
    pub fn snapshot(&self) -> Snapshot {
        self.telemetry().snapshot()
    }
    /// Stops processing; every later sample is the delayed original. Publishes
    /// the terminal receipt once; later calls return it unchanged.
    pub fn finish(&mut self, mode: StopMode) -> Terminal {
        self.finished = true;
        if let Some(terminal) = self.shared.terminal.get() {
            return *terminal;
        }
        let snapshot = self.snapshot();
        let terminal = Terminal {
            stop_mode: mode,
            retirement_timed_out: false,
            joined: true,
            drained: !snapshot.failed,
            input_samples: self.samples,
            full_input_frames: self.frames,
            partial_input_samples: self.phase,
            submitted_frames: self.frames,
            pending_input_frames: 0,
            snapshot,
        };
        let _ = self.shared.terminal.set(terminal);
        terminal
    }
    pub fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        let input = if input.is_finite() { input } else { 0. };
        let dry = self.dry[self.dry_position];
        self.dry[self.dry_position] = input;
        self.dry_position += 1;
        if self.dry_position == self.latency_samples() {
            self.dry_position = 0;
        }
        if self.finished || self.shared.failed.load(Ordering::Relaxed) {
            self.voice = None;
            return dry;
        }
        if !self.warmed {
            if self.model.warm_thread().is_err() {
                self.shared.failed.store(true, Ordering::Release);
                self.voice = None;
                return dry;
            }
            self.warmed = true;
        }
        let frame = self.geometry.frame_samples;
        self.assembling[self.phase] = input;
        let output = if self.playing_valid {
            self.voice = Some(self.score);
            self.playing[self.phase]
        } else {
            self.voice = None;
            dry
        };
        self.samples += 1;
        self.phase += 1;
        if self.phase == frame {
            let wall = metrics::now(false);
            let cpu = metrics::now(true);
            match self.model.process(&mut self.assembling[..frame]) {
                Ok(score) => {
                    self.playing[..frame].copy_from_slice(&self.assembling[..frame]);
                    self.score = score;
                    self.playing_valid = true;
                }
                Err(_) => {
                    self.shared.failed.store(true, Ordering::Release);
                    self.playing_valid = false;
                }
            }
            let budget = self.geometry.frame_budget_ns();
            self.shared.cpu.record(cpu, metrics::now(true), budget);
            self.shared.wall.record(wall, metrics::now(false), budget);
            self.shared.completed.fetch_add(1, Ordering::Release);
            self.frames += 1;
            self.phase = 0;
        }
        dry + (output - dry) * wet
    }
}
