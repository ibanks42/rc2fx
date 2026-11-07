//! A stand-in mixer for working on the app away from the hardware, built only
//! with the `mock-mixer` feature. It replaces the HID client under the same
//! `api/v1` requests, so everything above it is the real app.
//!
//! It behaves as a running native engine does: saves check their revision, a
//! change is acknowledged first and settles a moment later, and it only reports
//! meters while running. Settings live for one run of the app; each start is
//! the same demo setup. No audio exists: meters are generated, and marked
//! synthetic so the channel editor labels its live readings as simulated.
//!
//! It answers 404 to `api/v1/ssh`, as a mixer without SSH management does, so
//! the app never pins a made-up host key among the real ones.
use super::Session;
use rc2fx_core::{
    Band, BandType, Channel, Config, ConfigUpdate, DeviceState, DeviceVersions, DiagnosticsEvent,
    DiagnosticsEvents, DiagnosticsKind, EVENTS_PAGE, EngineMode, EnginePhase, EngineStatus,
    EventDetails, MIXER_PROTOCOL_VERSION, Meter, MeterType, Performance, PresetSummary,
    RoutingPhase, RoutingRequest, RoutingStatus, SAMPLE_RATE, SelectionPhase, SelectionRequest,
    SelectionStatus, StockInventory, StockSource, Target, TargetKind, TargetMeter, Timing,
    UsbLinkState, UsbStatus, VoiceGate,
};
use rc2fx_installer::control::{Failure, Reply, Request};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

/// What the app sees as the mixer's USB serial.
pub(super) const SERIAL: &str = "MOCK-MIXER";
/// How long a change takes to reach "running" again.
const SETTLE: Duration = Duration::from_millis(300);
const METER_PERIOD: Duration = Duration::from_millis(100);
const USB_PERIOD: Duration = Duration::from_secs(1);
/// Often enough that the diagnostics events have something to show.
const XRUN_PERIOD: Duration = Duration::from_secs(47);
const BLOCK_SIZE: u32 = 112;
const FIRMWARE: &str = "1.7.7";
const FIRMWARE_PROFILE: &str = "rodecaster-pro-ii-1.7.7";

/// The show's faders: source id, name, color and the path it feeds. These
/// paths carry a signal; every other input is silent, like an unused socket.
const SHOW: [(u16, &str, &str, &str); 5] = [
    (3, "Host", "fffdba0e", "mic-1"),
    (0, "Guest", "ff5ce1e6", "analog-1"),
    (7, "Music", "ffa78bfa", "usb-main"),
    (8, "Call", "fff17e7e", "usb-chat"),
    (9, "Phone", "ff929eaf", "bluetooth"),
];

/// Outlives connections, so settings survive Disconnect and Connect.
static MIXER: LazyLock<Mutex<Mixer>> = LazyLock::new(|| Mutex::new(Mixer::new(Instant::now())));

fn mixer() -> std::sync::MutexGuard<'static, Mixer> {
    MIXER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One app connection to the shared mixer.
pub(super) struct Connection;

impl Connection {
    pub(super) fn open() -> Self {
        mixer().connected();
        Self
    }
}

impl Session for Connection {
    fn request(&mut self, request: &Request) -> Result<Reply, Failure> {
        Ok(mixer().handle(request, Instant::now()))
    }
    fn is_open(&self) -> bool {
        true
    }
    fn reopen(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn reply(status: u16, body: &impl serde::Serialize) -> Reply {
    Reply {
        status,
        body: serde_json::to_value(body).expect("mock replies serialize"),
    }
}

fn failure(status: u16, message: impl std::fmt::Display) -> Reply {
    reply(status, &json!({"error": message.to_string()}))
}

fn parse<T: serde::de::DeserializeOwned>(body: Option<&Value>) -> Result<T, Reply> {
    let value = body.ok_or_else(|| failure(400, "A JSON body is required"))?;
    serde_json::from_value(value.clone()).map_err(|error| failure(400, error))
}

fn expected_revision(body: Option<&Value>) -> Result<u64, Reply> {
    body.and_then(|body| body["expectedRevision"].as_u64())
        .ok_or_else(|| failure(400, "expectedRevision is required"))
}

/// The demo's starting point: a treated host mic, so the editors have
/// something to show, and two more paths switched on.
fn demo_config() -> Config {
    let mut config = Config {
        selected_targets: ["mic-1", "analog-1", "usb-chat"]
            .map(str::to_owned)
            .to_vec(),
        ..Config::default()
    };
    let host = config
        .targets
        .iter_mut()
        .find(|target| target.id == "mic-1")
        .expect("the default catalog has mic-1");
    host.highpass.enabled = true;
    host.suppression.enabled = true;
    host.suppression.wet = 0.9;
    host.suppression.gate = VoiceGate::with_strength(0.4);
    host.eq.enabled = true;
    for (band_type, frequency, gain) in [
        (BandType::LowShelf, 120.0, -2.0),
        (BandType::Bell, 3_000.0, 2.5),
        (BandType::HighShelf, 10_000.0, 1.5),
    ] {
        let band: &mut Band = host
            .eq
            .add_band(Channel::Left)
            .expect("the demo stays under the band limit");
        band.band_type = band_type;
        band.frequency = frequency;
        band.gain = gain;
    }
    host.compressor.enabled = true;
    host.limiter.enabled = true;
    config
}

/// A level and whether it is speech, for one path at `seconds`.
fn signal(target: &Target, seconds: f64) -> (f64, bool) {
    // A silent mixer, to see what the app costs with nothing to show.
    if std::env::var_os("RC2FX_MOCK_SILENT").is_some() {
        return (0.0, false);
    }
    let seed = target.id.bytes().map(f64::from).sum::<f64>();
    let wave = |rate: f64, phase: f64| (seconds * rate + seed * phase).sin();
    let in_show = SHOW.iter().any(|(.., id)| *id == target.id);
    match target.kind {
        TargetKind::Mic if in_show => {
            let speaking = wave(0.9, 1.0) + wave(0.37, 3.0) > -0.3;
            if speaking {
                (0.12 + 0.2 * (1.0 + wave(9.0, 2.0) * wave(3.1, 5.0)), true)
            } else {
                (0.006 * (1.5 + wave(2.3, 7.0)), false)
            }
        }
        TargetKind::Playback if in_show => {
            (0.25 + 0.1 * wave(1.4, 1.0) + 0.08 * wave(8.0, 2.0), false)
        }
        TargetKind::Output => (0.3 + 0.1 * wave(1.1, 1.0) + 0.1 * wave(6.0, 2.0), false),
        _ => (0.0, false),
    }
}

fn decibels(level: f64) -> f64 {
    20.0 * level.max(1e-6).log10()
}

fn level(decibels: f64) -> f64 {
    10f64.powf(decibels / 20.0)
}

/// What the chain would roughly do to `input`: enough for each control to
/// visibly move the meters, not a model of the DSP.
fn process(target: &Target, input: f64, speaking: bool, meter: &mut TargetMeter) {
    let mut output = input;
    if target.suppression.enabled {
        let gate = &target.suppression.gate;
        meter.voice = if speaking { 0.92 } else { 0.04 };
        meter.gate_open = if speaking || !gate.active() { 1.0 } else { 0.0 };
        if !speaking {
            output *= 1.0 - 0.9 * target.suppression.wet;
            output *= level(-gate.shape().reduction_db);
        }
    }
    if target.eq.enabled {
        output *= level(target.eq.input_gain_db + target.eq.output_gain_db);
    }
    if target.compressor.enabled {
        let compressor = &target.compressor;
        let over = (decibels(output) - compressor.threshold).max(0.0);
        meter.gain_reduction = over * (1.0 - 1.0 / compressor.ratio);
        output *= level(compressor.makeup - meter.gain_reduction);
    }
    if target.gain.enabled {
        output *= level(target.gain.db);
    }
    if target.limiter.enabled {
        output = output.min(level(target.limiter.ceiling));
    }
    meter.output_peak = output;
    meter.rms = output * 0.4;
}

struct Mixer {
    config: Config,
    engine: EngineStatus,
    routing: RoutingStatus,
    selection: SelectionStatus,
    /// Global processing, independent of which paths are selected.
    processing: bool,
    settle_at: Option<Instant>,
    presets: Vec<(PresetSummary, Vec<Target>)>,
    next_preset: u64,
    /// State changes waiting for the next event poll.
    events: VecDeque<Value>,
    log: VecDeque<DiagnosticsEvent>,
    next_seq: u64,
    diagnostics_epoch: u64,
    started: Instant,
    last_meter: Instant,
    last_usb: Option<Instant>,
    last_xrun: Instant,
    /// Frames since the last diagnostics reset; every counter derives from it.
    frames: u64,
    xruns: u64,
}

impl Mixer {
    fn new(now: Instant) -> Self {
        let config = demo_config();
        let mut mixer = Self {
            engine: EngineStatus {
                generation: 1,
                restart_allowed: false,
                mode: EngineMode::Native,
                status: EnginePhase::Starting,
                applied_revision: 0,
                sample_rate: SAMPLE_RATE as u32,
                block_size: BLOCK_SIZE,
                message: String::new(),
                audio_advancing: false,
            },
            routing: RoutingStatus {
                status: RoutingPhase::Unmanaged,
                channels: 0,
                recovered: 0,
                message: String::new(),
            },
            selection: SelectionStatus {
                channel_capacity: None,
                status: SelectionPhase::Idle,
                allowed: false,
                qualified_target_ids: Vec::new(),
                active_target_ids: Vec::new(),
                pending_target_ids: Vec::new(),
                message: String::new(),
            },
            config,
            processing: true,
            settle_at: None,
            presets: Vec::new(),
            next_preset: 1,
            events: VecDeque::new(),
            log: VecDeque::new(),
            next_seq: 1,
            diagnostics_epoch: 0,
            started: now,
            last_meter: now,
            last_usb: None,
            last_xrun: now,
            frames: 0,
            xruns: 0,
        };
        mixer.settle();
        mixer
    }

    /// A new client's event stream starts from the current state.
    fn connected(&mut self) {
        self.events.clear();
        self.publish();
    }

    fn state(&self) -> DeviceState {
        let canonical = |ids: &[String]| self.config.canonical_selection(ids).unwrap_or_default();
        let mut selection = self.selection.clone();
        // Saved effects can reorder the catalog; these lists follow it.
        selection.qualified_target_ids = self.config.targets.iter().map(|t| t.id.clone()).collect();
        selection.active_target_ids = canonical(&selection.active_target_ids);
        selection.pending_target_ids = canonical(&selection.pending_target_ids);
        DeviceState {
            config: self.config.clone(),
            engine: self.engine.clone(),
            routing: self.routing.clone(),
            selection,
        }
    }

    fn publish(&mut self) {
        let data = serde_json::to_value(self.state()).expect("state serializes");
        self.events
            .push_back(json!({"event": "state", "data": data}));
    }

    /// A change was accepted; audio follows after `SETTLE`.
    fn begin_change(&mut self, now: Instant) {
        self.engine.status = EnginePhase::Starting;
        self.engine.audio_advancing = false;
        self.engine.applied_revision = 0;
        self.engine.message = "Settings saved; waiting for native audio acknowledgement".into();
        self.selection.allowed = false;
        self.settle_at = Some(now + SETTLE);
    }

    /// The engine has caught up with the saved settings.
    fn settle(&mut self) {
        self.settle_at = None;
        self.engine.status = EnginePhase::Running;
        self.engine.audio_advancing = true;
        self.engine.applied_revision = self.config.revision;
        self.engine.message = "Mock mixer: simulated audio".into();
        self.selection.status = SelectionPhase::Idle;
        self.selection.allowed = true;
        self.selection.pending_target_ids.clear();
        self.selection.active_target_ids = if self.processing {
            self.config.selected_targets.clone()
        } else {
            Vec::new()
        };
        self.routing.status = if self.processing {
            RoutingPhase::Active
        } else {
            RoutingPhase::Bypassed
        };
        self.routing.channels = if self.processing {
            self.config.selected_channels()
        } else {
            0
        };
    }

    fn record(
        &mut self,
        kind: DiagnosticsKind,
        message: &str,
        details: EventDetails,
        now: Instant,
    ) {
        self.log.push_back(DiagnosticsEvent {
            seq: self.next_seq,
            kind,
            path: None,
            at_ns: self.now_ns(now),
            message: message.into(),
            details,
        });
        self.next_seq += 1;
    }

    fn now_ns(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_nanos() as u64
    }

    /// Everything that happens with time alone, caught up on each request.
    fn advance(&mut self, now: Instant) {
        if self.settle_at.is_some_and(|at| now >= at) {
            self.settle();
            self.publish();
        }
        if self.engine.status == EnginePhase::Running
            && now.saturating_duration_since(self.last_xrun) >= XRUN_PERIOD
        {
            self.last_xrun = now;
            self.xruns += 1;
            let details = EventDetails {
                delayed_us: Some(180),
                ..EventDetails::default()
            };
            self.record(DiagnosticsKind::Xrun, "Simulated xrun", details, now);
        }
    }

    fn handle(&mut self, request: &Request, now: Instant) -> Reply {
        self.advance(now);
        let body = request.body.as_ref();
        let path = request.path.as_str();
        let result = match (request.method.as_str(), path) {
            ("GET", "api/v1/events") => Ok(reply(200, &self.next_event(now))),
            ("GET", "api/v1/state") => Ok(reply(200, &self.state())),
            ("GET", "api/v1/version") => Ok(reply(200, &versions())),
            ("GET", "api/v1/inputs") => Ok(reply(200, &inventory())),
            ("GET", "api/v1/diagnostics") => {
                Ok(reply(200, &json!({"mock": true, "state": self.state()})))
            }
            ("POST", "api/v1/diagnostics/reset") => Ok(self.reset_diagnostics(now)),
            ("GET", path) if path.starts_with("api/v1/diagnostics/events") => {
                let after = path
                    .split_once("after=")
                    .and_then(|(_, after)| after.parse().ok())
                    .unwrap_or(0);
                Ok(reply(200, &self.events_page(after, now)))
            }
            ("PUT", "api/v1/config") => {
                parse::<ConfigUpdate>(body).and_then(|update| self.save(update, now))
            }
            ("PUT", "api/v1/routing") => self.set_routing(body, now),
            ("PUT", "api/v1/selection") => self.select(body, now),
            ("POST", "api/v1/engine/restart") => Err(failure(
                422,
                "Native per-path updates do not restart the vendor audio engine",
            )),
            ("GET", "api/v1/presets") => {
                let presets: Vec<_> = self.presets.iter().map(|(summary, _)| summary).collect();
                Ok(reply(200, &json!({"presets": presets})))
            }
            ("POST", "api/v1/presets") => self.create_preset(body),
            ("POST", path) if path.ends_with("/apply") => self.apply_preset(path, body, now),
            ("DELETE", path) => self.delete_preset(path),
            _ => Err(failure(404, "Unknown endpoint")),
        };
        result.unwrap_or_else(|reply| reply)
    }

    fn check_revision(&self, revision: u64) -> Result<(), Reply> {
        if revision == self.config.revision {
            Ok(())
        } else {
            Err(failure(409, "revision conflict"))
        }
    }

    /// The checks every processing transition shares.
    fn check_transition(&self, revision: u64, generation: u64) -> Result<(), Reply> {
        self.check_revision(revision)?;
        if generation != self.engine.generation || !self.selection.allowed {
            return Err(failure(409, "a transition is already pending"));
        }
        Ok(())
    }

    fn save(&mut self, update: ConfigUpdate, now: Instant) -> Result<Reply, Reply> {
        self.check_revision(update.expected_revision)?;
        self.config
            .validate_update(&update.targets)
            .map_err(|error| failure(400, format!("invalid request: {error}")))?;
        self.config.revision += 1;
        self.config.targets = update.targets;
        self.begin_change(now);
        self.publish();
        Ok(reply(200, &self.state()))
    }

    fn set_routing(&mut self, body: Option<&Value>, now: Instant) -> Result<Reply, Reply> {
        let request: RoutingRequest = parse(body)?;
        self.check_transition(request.expected_revision, request.expected_generation)?;
        self.processing = request.enabled;
        self.begin_change(now);
        self.routing.status = if request.enabled {
            RoutingPhase::Starting
        } else {
            RoutingPhase::Bypassing
        };
        self.routing.channels = 0;
        self.publish();
        Ok(reply(202, &self.state()))
    }

    fn select(&mut self, body: Option<&Value>, now: Instant) -> Result<Reply, Reply> {
        let request: SelectionRequest = parse(body)?;
        self.check_transition(request.expected_revision, request.expected_generation)?;
        let ids = self
            .config
            .canonical_selection(&request.target_ids)
            .map_err(|error| failure(400, format!("invalid request: {error}")))?;
        self.config.revision += 1;
        self.config.selected_targets = ids.clone();
        self.begin_change(now);
        self.selection.status = SelectionPhase::Changing;
        self.selection.pending_target_ids = ids;
        self.routing.status = if self.processing {
            RoutingPhase::Starting
        } else {
            RoutingPhase::Bypassing
        };
        self.routing.channels = 0;
        self.publish();
        Ok(reply(202, &self.state()))
    }

    fn create_preset(&mut self, body: Option<&Value>) -> Result<Reply, Reply> {
        self.check_revision(expected_revision(body)?)?;
        let name = body
            .and_then(|body| body["name"].as_str())
            .ok_or_else(|| failure(400, "name is required"))?;
        let summary = PresetSummary {
            id: format!("preset-{}", self.next_preset),
            name: name.into(),
        };
        self.next_preset += 1;
        self.presets
            .push((summary.clone(), self.config.targets.clone()));
        Ok(reply(201, &summary))
    }

    fn apply_preset(
        &mut self,
        path: &str,
        body: Option<&Value>,
        now: Instant,
    ) -> Result<Reply, Reply> {
        let id = path
            .strip_prefix("api/v1/presets/")
            .and_then(|path| path.strip_suffix("/apply"));
        let (_, targets) = self
            .presets
            .iter()
            .find(|(summary, _)| Some(summary.id.as_str()) == id)
            .ok_or_else(|| failure(404, "not found"))?;
        let update = ConfigUpdate {
            expected_revision: expected_revision(body)?,
            targets: targets.clone(),
        };
        self.save(update, now)
    }

    fn delete_preset(&mut self, path: &str) -> Result<Reply, Reply> {
        let id = path
            .strip_prefix("api/v1/presets/")
            .ok_or_else(|| failure(404, "Unknown endpoint"))?;
        let index = self
            .presets
            .iter()
            .position(|(summary, _)| summary.id == id)
            .ok_or_else(|| failure(404, "not found"))?;
        self.presets.remove(index);
        Ok(reply(200, &json!({"deleted": id})))
    }

    fn reset_diagnostics(&mut self, now: Instant) -> Reply {
        self.diagnostics_epoch += 1;
        self.frames = 0;
        self.xruns = 0;
        self.log.clear();
        let details = EventDetails::default();
        self.record(DiagnosticsKind::Reset, "Diagnostics reset", details, now);
        reply(200, &self.state())
    }

    fn events_page(&self, after: u64, now: Instant) -> DiagnosticsEvents {
        let events: Vec<_> = self
            .log
            .iter()
            .filter(|event| event.seq > after)
            .take(EVENTS_PAGE)
            .cloned()
            .collect();
        DiagnosticsEvents {
            now_ns: self.now_ns(now),
            next_seq: events
                .last()
                .map_or(self.next_seq.max(after), |last| last.seq + 1),
            lost: 0,
            more: events
                .last()
                .is_some_and(|last| last.seq + 1 < self.next_seq),
            events,
        }
    }

    /// State changes first, then whichever periodic report is due.
    fn next_event(&mut self, now: Instant) -> Value {
        if let Some(event) = self.events.pop_front() {
            return event;
        }
        if self
            .last_usb
            .is_none_or(|last| now.saturating_duration_since(last) >= USB_PERIOD)
        {
            self.last_usb = Some(now);
            let usb = UsbStatus {
                usb1: UsbLinkState::Configured,
            };
            return json!({"event": "usb", "data": usb});
        }
        let elapsed = now.saturating_duration_since(self.last_meter);
        if self.engine.status == EnginePhase::Running && elapsed >= METER_PERIOD {
            self.last_meter = now;
            self.frames += (elapsed.as_secs_f64() * SAMPLE_RATE) as u64;
            return json!({"event": "meter", "data": self.meter(now)});
        }
        Value::Null
    }

    fn meter(&self, now: Instant) -> Meter {
        let seconds = now.saturating_duration_since(self.started).as_secs_f64();
        Meter {
            message_type: MeterType::Meter,
            revision: self.engine.applied_revision,
            generation: self.engine.generation,
            synthetic: true,
            diagnostics_epoch: self.diagnostics_epoch,
            targets: self
                .config
                .targets
                .iter()
                .map(|target| self.target_meter(target, seconds))
                .collect(),
        }
    }

    fn target_meter(&self, target: &Target, seconds: f64) -> TargetMeter {
        let active = self.selection.active_target_ids.contains(&target.id);
        let suppressing = active && target.suppression.enabled;
        let (input, speaking) = signal(target, seconds);
        let block_size = u64::from(BLOCK_SIZE);
        let suppression_frames = if suppressing {
            self.frames / u64::from(rc2fx_core::DFN_HOP_SAMPLES)
        } else {
            0
        };
        let mut meter = TargetMeter {
            id: target.id.clone(),
            input_peak: input,
            output_peak: input,
            rms: input * 0.4,
            gain_reduction: 0.0,
            voice: 0.0,
            gate_open: 0.0,
            clipped: 0,
            spectrum: Vec::new(),
            callbacks: self.frames / block_size,
            processed_frames: self.frames,
            suppression_frames,
            missed_deadlines: 0,
            stale_results: 0,
            input_drops: 0,
            output_drops: 0,
            latency_samples: if suppressing {
                target
                    .suppression
                    .geometry()
                    .map_or(0, |geometry| geometry.latency_samples())
            } else {
                0
            },
            worker_failed: false,
            performance: active.then(|| self.performance(suppression_frames)),
        };
        if active && !target.bypass {
            process(target, input, speaking, &mut meter);
        }
        meter
    }

    fn performance(&self, worker_frames: u64) -> Performance {
        let timing = |calls: u64, mean_ns: u64, max_ns: u64| Timing {
            calls,
            total_ns: calls * mean_ns,
            max_ns: if calls == 0 { 0 } else { max_ns },
            overruns: 0,
            clock_errors: 0,
            histogram: [calls, 0, 0, 0, 0, 0, 0, 0],
        };
        Performance {
            callback_budget_ns: 2_333_333,
            worker_budget_ns: 9_333_333,
            xruns: self.xruns,
            callback: timing(self.frames / u64::from(BLOCK_SIZE), 420_000, 910_000),
            worker_wall: timing(worker_frames, 2_100_000, 3_800_000),
            worker_cpu: timing(worker_frames, 1_900_000, 3_400_000),
            helper_late: Some(0),
            helper_stalls: Some(0),
        }
    }
}

fn versions() -> DeviceVersions {
    DeviceVersions {
        runtime: Some(env!("CARGO_PKG_VERSION").into()),
        protocol: Some(MIXER_PROTOCOL_VERSION),
        firmware: Some(FIRMWARE.into()),
        firmware_profile: Some(FIRMWARE_PROFILE.into()),
    }
}

fn inventory() -> StockInventory {
    StockInventory {
        available: true,
        show_name: "Mock Show".into(),
        revision: "0".repeat(64),
        message: String::new(),
        inputs: SHOW
            .iter()
            .zip(1..)
            .map(|(&(source_id, name, color, target), fader)| StockSource {
                id: format!("input-{source_id}"),
                source_id,
                fader: Some(fader),
                name: name.into(),
                color: color.into(),
                target_id: target.into(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::Session as AppSession;

    fn request(method: &str, path: &str, body: Option<Value>) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            body,
        }
    }

    /// A mixer and the app's own session model, connected the way the app is.
    fn connected() -> (Mixer, AppSession, Instant) {
        let now = Instant::now();
        let mut mixer = Mixer::new(now);
        mixer.connected();
        let mut session = AppSession::default();
        session.receive(read_state(&mut mixer, now)).unwrap();
        (mixer, session, now)
    }

    /// Reads state through the app's strict parser.
    fn read_state(mixer: &mut Mixer, now: Instant) -> DeviceState {
        let reply = mixer.handle(&request("GET", "api/v1/state", None), now);
        assert_eq!(reply.status, 200);
        DeviceState::from_json(&reply.body.to_string()).unwrap()
    }

    fn settled(mixer: &mut Mixer, now: Instant) -> (DeviceState, Instant) {
        let later = now + SETTLE;
        (read_state(mixer, later), later)
    }

    /// Polls until a meter arrives, as the app's event stream does.
    fn next_meter(mixer: &mut Mixer, mut now: Instant) -> (Meter, Instant) {
        for _ in 0..100 {
            now += Duration::from_millis(50);
            let reply = mixer.handle(&request("GET", "api/v1/events", None), now);
            if reply.body["event"] == "meter" {
                return (
                    serde_json::from_value(reply.body["data"].clone()).unwrap(),
                    now,
                );
            }
        }
        panic!("no meter arrived");
    }

    fn assert_meter_accepted(meter: &Meter, state: &DeviceState) {
        assert_eq!(meter.generation, state.engine.generation);
        assert_eq!(meter.revision, state.engine.applied_revision);
        assert!(meter.synthetic, "the app must label this as a simulation");
        let ids = meter.targets.iter().map(|target| &target.id);
        assert!(ids.eq(state.selection.qualified_target_ids.iter()));
        for target in &meter.targets {
            target.validate().unwrap();
        }
    }

    /// The whole way through `Transport`, as the app sends it. The only test
    /// using the shared mixer; the others each build their own.
    #[test]
    fn a_save_goes_through_the_transport_and_comes_back_as_meters() {
        use crate::{Command, EventKind, Transport};
        fn wait<T>(transport: &mut Transport, mut found: impl FnMut(EventKind) -> Option<T>) -> T {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match transport.try_recv() {
                    Some(event) => {
                        if let Some(value) = found(event.kind) {
                            return value;
                        }
                    }
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            panic!("the expected event did not arrive");
        }
        let mut transport = Transport::new().unwrap();
        transport.send(Command::Connect { generation: 1 }).unwrap();
        let (mixer, state) = wait(&mut transport, |kind| match kind {
            EventKind::Connected { mixer, state } => Some((mixer, state)),
            _ => None,
        });
        assert_eq!(mixer, SERIAL);
        let state = DeviceState::from_json(&state.to_string()).unwrap();
        let update = ConfigUpdate {
            expected_revision: state.config.revision,
            targets: state.config.targets.clone(),
        };
        let command = Command::Apply {
            generation: 1,
            update,
        };
        transport.send(command).unwrap();
        let revision = state.config.revision + 1;
        wait(&mut transport, |kind| match kind {
            EventKind::Applied(state) => Some(assert_eq!(
                DeviceState::from_json(&state.to_string())
                    .unwrap()
                    .config
                    .revision,
                revision
            )),
            _ => None,
        });
        wait(&mut transport, |kind| match kind {
            EventKind::Meter(meter) => (meter["revision"] == revision).then_some(()),
            _ => None,
        });
    }

    #[test]
    fn the_app_accepts_the_starting_state_as_applied_and_ready() {
        let (mut mixer, session, now) = connected();
        let state = read_state(&mut mixer, now);
        assert!(state.is_applied());
        assert!(state.can_disable_processing());
        assert!(session.is_applied());
        let (meter, _) = next_meter(&mut mixer, now);
        assert_meter_accepted(&meter, &state);
    }

    #[test]
    fn a_new_connection_receives_the_state_first() {
        let (mut mixer, _, now) = connected();
        let reply = mixer.handle(&request("GET", "api/v1/events", None), now);
        assert_eq!(reply.body["event"], "state");
        DeviceState::from_json(&reply.body["data"].to_string()).unwrap();
    }

    #[test]
    fn a_save_is_acknowledged_then_settles_and_a_stale_revision_conflicts() {
        let (mut mixer, mut session, now) = connected();
        session.draft[0].gain.enabled = true;
        session.draft[0].gain.db = 6.0;
        let pending = session.begin_apply().unwrap();
        let body = serde_json::to_value(&pending.request).unwrap();
        let reply = mixer.handle(&request("PUT", "api/v1/config", Some(body.clone())), now);
        assert_eq!(reply.status, 200);
        let state = DeviceState::from_json(&reply.body.to_string()).unwrap();
        assert!(state.processing_transition_pending());
        session.acknowledge(&pending, state).unwrap();

        let (state, later) = settled(&mut mixer, now);
        assert!(state.is_applied());
        assert_eq!(state.config.targets[0].gain.db, 6.0);
        let conflict = mixer.handle(&request("PUT", "api/v1/config", Some(body)), later);
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.body["error"], "revision conflict");
    }

    #[test]
    fn a_save_the_real_mixer_would_refuse_is_refused() {
        let (mut mixer, session, now) = connected();
        let mut targets = session.draft.clone();
        targets[0].gain.db = 99.0;
        let update = ConfigUpdate {
            expected_revision: mixer.config.revision,
            targets,
        };
        let body = serde_json::to_value(&update).unwrap();
        let reply = mixer.handle(&request("PUT", "api/v1/config", Some(body)), now);
        assert_eq!(reply.status, 400);
    }

    #[test]
    fn a_path_change_passes_the_apps_acknowledgement_check_and_settles() {
        let (mut mixer, mut session, now) = connected();
        session.selection_draft.push("usb-main".into());
        let pending = session.begin_selection().unwrap();
        let body = serde_json::to_value(&pending.request).unwrap();
        let reply = mixer.handle(&request("PUT", "api/v1/selection", Some(body)), now);
        assert_eq!(reply.status, 202);
        let state = DeviceState::from_json(&reply.body.to_string()).unwrap();
        pending.validate_acknowledgement(&state).unwrap();

        let (state, later) = settled(&mut mixer, now);
        assert!(state.is_applied());
        assert!(
            state
                .selection
                .active_target_ids
                .contains(&"usb-main".to_owned())
        );
        let (meter, _) = next_meter(&mut mixer, later);
        assert_meter_accepted(&meter, &state);
    }

    #[test]
    fn processing_turns_off_and_on_through_the_apps_transitions() {
        let (mut mixer, mut session, now) = connected();
        let pending = session.begin_routing(false).unwrap();
        let body = serde_json::to_value(&pending.request).unwrap();
        let reply = mixer.handle(&request("PUT", "api/v1/routing", Some(body)), now);
        assert_eq!(reply.status, 202);
        let state = DeviceState::from_json(&reply.body.to_string()).unwrap();
        pending.validate_acknowledgement(&state).unwrap();

        let (state, later) = settled(&mut mixer, now);
        assert!(state.selection.active_target_ids.is_empty());
        assert!(state.can_enable_processing());
        session.receive(state).unwrap();
        let pending = session.begin_routing(true).unwrap();
        let body = serde_json::to_value(&pending.request).unwrap();
        let reply = mixer.handle(&request("PUT", "api/v1/routing", Some(body)), later);
        let state = DeviceState::from_json(&reply.body.to_string()).unwrap();
        pending.validate_acknowledgement(&state).unwrap();
        let (state, _) = settled(&mut mixer, later);
        assert!(state.is_applied());
    }

    #[test]
    fn a_second_transition_waits_for_the_first() {
        let (mut mixer, session, now) = connected();
        let saved = session.saved.as_ref().unwrap();
        let body = |enabled| {
            Some(json!({
                "enabled": enabled,
                "expectedRevision": saved.config.revision,
                "expectedGeneration": saved.engine.generation,
            }))
        };
        let first = mixer.handle(&request("PUT", "api/v1/routing", body(false)), now);
        assert_eq!(first.status, 202);
        let second = mixer.handle(&request("PUT", "api/v1/routing", body(true)), now);
        assert_eq!(second.status, 409);
    }

    #[test]
    fn meters_stop_while_a_change_settles() {
        let (mut mixer, session, now) = connected();
        let update = ConfigUpdate {
            expected_revision: mixer.config.revision,
            targets: session.draft.clone(),
        };
        let body = serde_json::to_value(&update).unwrap();
        mixer.handle(&request("PUT", "api/v1/config", Some(body)), now);
        mixer.events.clear();
        mixer.last_usb = Some(now);
        let during = now + METER_PERIOD;
        let event = mixer.handle(&request("GET", "api/v1/events", None), during);
        assert!(event.body.is_null());
    }

    #[test]
    fn the_controls_move_the_meters() {
        let (mut mixer, mut session, now) = connected();
        let index = |mixer: &Mixer| {
            mixer
                .config
                .targets
                .iter()
                .position(|t| t.id == "analog-1")
                .unwrap()
        };
        let peak = |meter: &Meter| {
            let target = meter.targets.iter().find(|t| t.id == "analog-1").unwrap();
            (target.input_peak, target.output_peak)
        };
        let (meter, now) = next_meter(&mut mixer, now);
        let (input, output) = peak(&meter);
        assert_eq!(input, output, "no effects enabled yet");
        let target = index(&mixer);
        session.draft[target].gain.enabled = true;
        session.draft[target].gain.db = -12.0;
        let pending = session.begin_apply().unwrap();
        let body = serde_json::to_value(&pending.request).unwrap();
        mixer.handle(&request("PUT", "api/v1/config", Some(body)), now);
        let (_, now) = settled(&mut mixer, now);
        let (meter, _) = next_meter(&mut mixer, now);
        let (input, output) = peak(&meter);
        assert!(output < input * 0.3, "{output} from {input}");
    }

    #[test]
    fn presets_save_apply_and_delete() {
        let (mut mixer, _, now) = connected();
        let revision = mixer.config.revision;
        let created = mixer.handle(
            &request(
                "POST",
                "api/v1/presets",
                Some(json!({"name": "Voice", "expectedRevision": revision})),
            ),
            now,
        );
        assert_eq!(created.status, 201);
        let id = created.body["id"].as_str().unwrap().to_owned();
        assert!(rc2fx_core::valid_id(&id));
        let list = mixer.handle(&request("GET", "api/v1/presets", None), now);
        assert_eq!(list.body["presets"][0]["name"], "Voice");

        let apply = format!("api/v1/presets/{id}/apply");
        let body = json!({"expectedRevision": revision});
        let applied = mixer.handle(&request("POST", &apply, Some(body)), now);
        assert_eq!(applied.status, 200);
        DeviceState::from_json(&applied.body.to_string()).unwrap();
        assert_eq!(mixer.config.revision, revision + 1);

        let path = format!("api/v1/presets/{id}");
        let deleted = mixer.handle(&request("DELETE", &path, None), now);
        assert_eq!(deleted.body, json!({"deleted": id}));
        let missing = mixer.handle(&request("DELETE", &path, None), now);
        assert_eq!(missing.status, 404);
    }

    #[test]
    fn inputs_and_versions_pass_the_apps_checks() {
        let (mut mixer, _, now) = connected();
        let reply = mixer.handle(&request("GET", "api/v1/inputs", None), now);
        let inventory: StockInventory = serde_json::from_value(reply.body).unwrap();
        inventory.validate().unwrap();
        let reply = mixer.handle(&request("GET", "api/v1/version", None), now);
        let versions: DeviceVersions = serde_json::from_value(reply.body).unwrap();
        assert_eq!(versions.firmware_profile.as_deref(), Some(FIRMWARE_PROFILE));
    }

    #[test]
    fn ssh_is_reported_as_unsupported() {
        let (mut mixer, _, now) = connected();
        let reply = mixer.handle(&request("GET", "api/v1/ssh", None), now);
        assert_eq!(reply.status, 404);
    }

    #[test]
    fn a_diagnostics_reset_restarts_the_counters_under_a_new_epoch() {
        let (mut mixer, _, now) = connected();
        let (before, now) = next_meter(&mut mixer, now);
        assert!(before.targets[0].processed_frames > 0);
        let reply = mixer.handle(&request("POST", "api/v1/diagnostics/reset", None), now);
        assert_eq!(reply.status, 200);
        let (after, now) = next_meter(&mut mixer, now);
        assert_eq!(after.diagnostics_epoch, before.diagnostics_epoch + 1);
        assert!(after.targets[0].processed_frames <= before.targets[0].processed_frames);
        let path = "api/v1/diagnostics/events?after=0";
        let page = mixer.handle(&request("GET", path, None), now);
        let page: DiagnosticsEvents = serde_json::from_value(page.body).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].kind, DiagnosticsKind::Reset);
    }

    #[test]
    fn xruns_appear_in_the_event_log_and_the_meter() {
        let (mut mixer, _, now) = connected();
        let later = now + XRUN_PERIOD;
        let page = mixer.handle(&request("GET", "api/v1/diagnostics/events", None), later);
        let page: DiagnosticsEvents = serde_json::from_value(page.body).unwrap();
        assert_eq!(page.events[0].kind, DiagnosticsKind::Xrun);
        let (meter, _) = next_meter(&mut mixer, later);
        let host = meter.targets.iter().find(|t| t.id == "mic-1").unwrap();
        assert_eq!(host.performance.as_ref().unwrap().xruns, 1);
    }
}
