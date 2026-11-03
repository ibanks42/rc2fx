//! Control-side provider ownership and exact retirement evidence. The existing
//! Rust worker is the callback endpoint; inference never runs in this module.
use rc2fx_dsp::Suppression;
use rc2fx_suppression::{Endpoint, Telemetry, Termination, WorkerOptions};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    cell::UnsafeCell,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Shared by the fixed bound slots in one pipeline. A staged replacement may
/// coexist with its own slot, but cannot admit a second microphone target.
#[derive(Clone, Default)]
pub struct Admission(Arc<Mutex<Option<(String, usize)>>>);
pub(super) struct Claim(Admission);
impl Admission {
    pub(super) fn claim(&self, id: &str) -> Result<Claim, super::parameters::Error> {
        let mut value = self
            .0
            .lock()
            .map_err(|_| super::parameters::Error::Runtime)?;
        match value.as_mut() {
            Some((current, count)) if current == id => *count += 1,
            Some(_) => return Err(super::parameters::Error::SuppressionInUse),
            None => *value = Some((id.into(), 1)),
        }
        Ok(Claim(self.clone()))
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        let Ok(mut value) = self.0.0.lock() else {
            std::process::abort()
        };
        let Some((_, count)) = value.as_mut() else {
            std::process::abort()
        };
        *count -= 1;
        if *count == 0 {
            *value = None;
        }
    }
}
/// Control owns this allocation. The path's Reader may mutate the processor
/// only while its immutable descriptor is held for a frame: from the callback,
/// or from the pipeline's helper while the callback has handed that frame off.
pub struct Runtime {
    state: UnsafeCell<Processor>,
    worker: Option<Record>,
    epoch: u64,
    _claim: Option<Claim>,
    #[cfg(test)]
    retirement_blocked: std::sync::atomic::AtomicBool,
}
// No control code touches State until the reader releases a newer descriptor.
// Telemetry uses its existing atomic/SPSC ownership independently of State.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}
enum Processor {
    Ordinary(Box<rc2fx_dsp::Dsp>),
    Chat(Box<rc2fx_dsp::Chat>),
}
impl Runtime {
    pub fn new(
        target: &rc2fx_core::Target,
        prepared: &rc2fx_dsp::Prepared,
        options: WorkerOptions,
        epoch: u64,
        admission: &Admission,
    ) -> Result<Arc<Self>, super::parameters::Error> {
        let claim = if target.suppression.enabled {
            Some(admission.claim(&target.id)?)
        } else {
            None
        };
        let (mut providers, worker) = if target.suppression.enabled {
            let (provider, record) = worker(
                target.suppression.lookahead,
                options,
            )
            .map_err(|_| super::parameters::Error::Runtime)?;
            ([Some(provider), None], Some(record))
        } else {
            ([None, None], None)
        };
        let dsp = if prepared.is_chat_voice() {
            Processor::Chat(Box::new(
                rc2fx_dsp::Chat::new(
                    prepared,
                    providers[0]
                        .take()
                        .ok_or(super::parameters::Error::Runtime)?,
                )
                .map_err(|_| super::parameters::Error::Runtime)?,
            ))
        } else {
            Processor::Ordinary(Box::new(
                rc2fx_dsp::Dsp::new(prepared, providers)
                    .map_err(|_| super::parameters::Error::Runtime)?,
            ))
        };
        Ok(Arc::new(Self {
            state: UnsafeCell::new(dsp),
            worker,
            epoch,
            _claim: claim,
            #[cfg(test)]
            retirement_blocked: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn worker(&self) -> Option<&Record> {
        self.worker.as_ref()
    }
    /// Only control, after all callback descriptor loans have ended. A timeout
    /// retains the entire runtime in its bounded quarantine slot for retry.
    pub fn retire(&self) -> bool {
        #[cfg(test)]
        if self.retirement_blocked.load(Ordering::Relaxed) {
            return false;
        }
        let dsp = unsafe { &mut *self.state.get() };
        let _ = match dsp {
            Processor::Ordinary(dsp) => dsp.drain_suppression(Duration::from_secs(1)),
            Processor::Chat(dsp) => dsp.drain_suppression(Duration::from_secs(1)),
        };
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.0.termination() == Termination::Joined)
    }
    #[cfg(test)]
    pub fn block_retirement(&self, blocked: bool) {
        self.retirement_blocked.store(blocked, Ordering::Relaxed);
    }
    pub unsafe fn input(
        &self,
        prepared: &rc2fx_dsp::Prepared,
        input: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        match unsafe { &mut *self.state.get() } {
            Processor::Ordinary(dsp) => dsp.process(prepared, input, output),
            Processor::Chat(_) => Err(rc2fx_dsp::DspError::MissingChatStem),
        }
    }
    pub unsafe fn output(
        &self,
        prepared: &rc2fx_dsp::Prepared,
        buffers: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        match unsafe { &mut *self.state.get() } {
            Processor::Ordinary(dsp) => dsp.process_in_place(prepared, buffers),
            Processor::Chat(_) => Err(rc2fx_dsp::DspError::MissingChatStem),
        }
    }
    pub unsafe fn chat_output(
        &self,
        prepared: &rc2fx_dsp::Prepared,
        voice: [&[f32]; 2],
        buffers: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        match unsafe { &mut *self.state.get() } {
            Processor::Chat(dsp) => dsp.process(prepared, voice, buffers),
            Processor::Ordinary(_) => Err(rc2fx_dsp::DspError::MissingChatStem),
        }
    }
    pub unsafe fn latency(&self) -> usize {
        match unsafe { &*self.state.get() } {
            Processor::Ordinary(dsp) => dsp.latency_samples(),
            Processor::Chat(dsp) => dsp.latency_samples(),
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // Final teardown cannot unload code under an unresolved worker. Normal
        // retirement retains unresolved runtimes instead of reaching Drop.
        if !self.retire() {
            std::process::abort();
        }
    }
}

struct Provider(Endpoint);
impl Suppression for Provider {
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        Endpoint::geometry(&self.0).resolved()
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        self.0.process_sample(input, wet)
    }
    fn voice(&self) -> Option<f32> {
        self.0.voice()
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), rc2fx_dsp::RetirementError> {
        Suppression::drain(&mut self.0, timeout)
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        // Also covers partial construction failure before a Runtime exists.
        let _ = self
            .0
            .finish(rc2fx_suppression::StopMode::Drain, Duration::from_secs(1));
        if self.0.telemetry().termination() != Termination::Joined {
            std::process::abort();
        }
    }
}

static CREATED: AtomicU64 = AtomicU64::new(0);
static JOINED: AtomicU64 = AtomicU64::new(0);
static DRAINED: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static LATE: AtomicU64 = AtomicU64::new(0);
static INPUT_DROPS: AtomicU64 = AtomicU64::new(0);
static OUTPUT_DROPS: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static DELAY: AtomicU64 = AtomicU64::new(0);

/// Declare after its DSP owner so that provider retirement precedes this check.
/// An unresolved worker must never survive unloading its owning audit library.
pub struct Record(Telemetry, rc2fx_suppression::Geometry);
impl Record {
    pub fn telemetry(&self) -> Telemetry {
        self.0.clone()
    }
    pub fn geometry(&self) -> rc2fx_suppression::Geometry {
        self.1
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        if self.0.termination() != Termination::Joined {
            // Research process only. A qualified independent stock-launch
            // recovery owner is mandatory before any physical native trial.
            std::process::abort();
        }
        let Some(terminal) = self.0.terminal() else {
            std::process::abort();
        };
        JOINED.fetch_add(1, Ordering::Relaxed);
        DRAINED.fetch_add(u64::from(terminal.drained), Ordering::Relaxed);
        FRAMES.fetch_add(terminal.snapshot.completed_frames, Ordering::Relaxed);
        LATE.fetch_add(terminal.snapshot.missed_deadlines, Ordering::Relaxed);
        INPUT_DROPS.fetch_add(terminal.snapshot.input_drops, Ordering::Relaxed);
        OUTPUT_DROPS.fetch_add(terminal.snapshot.output_drops, Ordering::Relaxed);
        FAILED.fetch_add(u64::from(terminal.snapshot.failed), Ordering::Relaxed);
    }
}
pub fn worker(
    lookahead: u8,
    options: WorkerOptions,
) -> Result<(Box<dyn Suppression>, Record), String> {
    let worker = match Endpoint::new_with_affinity(
        lookahead,
        options,
        rc2fx_suppression::AffinityScope::KernelAllowed,
    ) {
        Ok(worker) => worker,
        Err(rc2fx_suppression::Error::StartupTerminationUnresolved) => std::process::abort(),
        Err(error) => return Err(error.to_string()),
    };
    let record = Record(worker.telemetry(), worker.geometry());
    CREATED.fetch_add(1, Ordering::Relaxed);
    DELAY.store(worker.latency_samples() as u64, Ordering::Relaxed);
    Ok((Box::new(Provider(worker)), record))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    created: u64,
    joined: u64,
    drained: u64,
    completed_frames: u64,
    late_frames: u64,
    input_drops: u64,
    output_drops: u64,
    failed: u64,
    latency_samples: u64,
}
pub fn stats() -> Stats {
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    Stats {
        created: read(&CREATED),
        joined: read(&JOINED),
        drained: read(&DRAINED),
        completed_frames: read(&FRAMES),
        late_frames: read(&LATE),
        input_drops: read(&INPUT_DROPS),
        output_drops: read(&OUTPUT_DROPS),
        failed: read(&FAILED),
        latency_samples: read(&DELAY),
    }
}

/// Non-RT report only. These are individually atomic observations, not a
/// coherent transaction. The CPU sum covers process and preparation windows,
/// not every instruction executed by the worker thread.
pub fn preparation_report(value: rc2fx_suppression::Preparation) -> serde_json::Value {
    use crate::parameters::worker_timing;
    serde_json::json!({
        "buildEnabled": value.enabled,
        "passWall": worker_timing(value.pass_wall),
        "passCpu": worker_timing(value.pass_cpu),
        "stepWall": worker_timing(value.step_wall),
        "steps": value.steps,
        "skipped": value.skipped,
        "abandoned": value.abandoned,
        "processAndPreparationCpuNs": value.total_cpu_ns,
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn preparation_report_distinguishes_shifted_work_from_process_cpu() {
        let mut value = rc2fx_suppression::Preparation {
            enabled: true,
            steps: 5,
            total_cpu_ns: 226,
            ..Default::default()
        };
        value.pass_cpu.total_ns = 21;
        value.step_wall.max_ns = 9;
        let report = super::preparation_report(value);
        assert_eq!(report["buildEnabled"], true);
        assert_eq!(report["steps"], 5);
        assert_eq!(report["passCpu"]["totalNs"], 21);
        assert_eq!(report["stepWall"]["maxNs"], 9);
        assert_eq!(report["processAndPreparationCpuNs"], 226);
    }
}
