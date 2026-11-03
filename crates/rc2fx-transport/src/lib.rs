//! RC2FX control over USB 1, owned by the desktop process. It never starts a
//! local service, uses the network or requires credentials.
mod usb_control;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use usb_control::Usb;

/// The largest request body the app sends.
const REQUEST_LIMIT: usize = 512 * 1024;

pub enum Command {
    Selection {
        generation: u64,
        request: rc2fx_core::SelectionRequest,
    },
    RestartEngine {
        generation: u64,
        request: rc2fx_core::EngineRestartRequest,
    },
    Diagnostics {
        generation: u64,
    },
    /// Restart the diagnostics baselines and the event log.
    ResetDiagnostics {
        generation: u64,
    },
    /// One page of the diagnostics event log, from `after` (a sequence
    /// number; 0 for everything the mixer still holds).
    DiagnosticsEvents {
        generation: u64,
        after: u64,
    },
    Inputs {
        generation: u64,
    },
    Routing {
        generation: u64,
        request: rc2fx_core::RoutingRequest,
    },
    Preset {
        generation: u64,
        action: PresetAction,
    },
    /// Connect to the mixer on USB 1.
    Connect {
        generation: u64,
    },
    Disconnect {
        generation: u64,
    },
    Apply {
        generation: u64,
        update: rc2fx_core::ConfigUpdate,
    },
    Refresh {
        generation: u64,
    },
    /// Read RC2FX's settings file, for the installer to keep.
    Settings {
        generation: u64,
    },
}

#[derive(Debug)]
pub struct Event {
    pub generation: u64,
    pub kind: EventKind,
}

#[derive(Debug)]
pub enum EventKind {
    Selection(Result<rc2fx_core::DeviceState, String>),
    RestartEngine(Result<rc2fx_core::DeviceState, String>),
    Diagnostics(Result<Value, String>),
    ResetDiagnostics(Result<Value, String>),
    DiagnosticsEvents(Result<rc2fx_core::DiagnosticsEvents, String>),
    Settings(Result<String, String>),
    Inputs(Result<rc2fx_core::StockInventory, String>),
    Routing(Result<rc2fx_core::DeviceState, String>),
    Preset(Result<PresetReply, PresetError>),
    /// Connected to the mixer with this USB serial; its state.
    Connected {
        mixer: String,
        state: Value,
    },
    Applied(Value),
    State(Value),
    Refreshed(Value),
    Engine(Value),
    Meter(Value),
    Usb(Value),
    StreamStatus {
        connected: bool,
        message: String,
    },
    Failure(String),
    Conflict,
}

#[derive(Debug)]
pub enum PresetAction {
    List,
    Create {
        name: String,
        expected_revision: u64,
    },
    Apply {
        id: String,
        expected_revision: u64,
    },
    Delete {
        id: String,
    },
}

#[derive(Debug)]
pub enum PresetReply {
    List(Vec<rc2fx_core::PresetSummary>),
    Created(rc2fx_core::PresetSummary),
    Applied(Value),
    Deleted(String),
}

#[derive(Debug)]
pub struct PresetError {
    pub message: String,
    pub conflict: bool,
}

fn preset_path(id: &str, apply: bool) -> Result<String, RequestError> {
    if !rc2fx_core::valid_id(id) {
        return Err(RequestError::Protocol);
    }
    Ok(format!(
        "api/v1/presets/{id}{}",
        if apply { "/apply" } else { "" }
    ))
}

fn preset_summary(value: Value) -> Result<rc2fx_core::PresetSummary, RequestError> {
    let summary: rc2fx_core::PresetSummary =
        serde_json::from_value(value).map_err(|_| RequestError::Protocol)?;
    if !rc2fx_core::valid_id(&summary.id)
        || summary.name.trim().is_empty()
        || summary.name.len() > 128
    {
        return Err(RequestError::Protocol);
    }
    Ok(summary)
}

fn preset_list(value: Value) -> Result<Vec<rc2fx_core::PresetSummary>, RequestError> {
    let object = value
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or(RequestError::Protocol)?;
    let list = object
        .get("presets")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 128)
        .ok_or(RequestError::Protocol)?;
    let mut ids = std::collections::HashSet::new();
    list.iter()
        .map(|v| {
            let preset = preset_summary(v.clone())?;
            if !ids.insert(preset.id.clone()) {
                return Err(RequestError::Protocol);
            }
            Ok(preset)
        })
        .collect()
}

pub struct Transport {
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    shutdown: watch::Sender<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Transport {
    pub fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("Device I/O runtime: {e}"))?;
        let (commands, receiver) = mpsc::channel(16);
        let (sender, events) = mpsc::channel(64);
        let (shutdown, mut stopped) = watch::channel(false);
        let worker = std::thread::Builder::new()
            .name("rc2fx-device-io".into())
            .spawn(move || {
                runtime.block_on(async move {
                    tokio::select! {
                        biased;
                        _ = stopped.changed() => {},
                        _ = run(receiver, sender) => {},
                    }
                })
            })
            .map_err(|e| format!("Device I/O thread: {e}"))?;
        Ok(Self {
            commands,
            events,
            shutdown,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .try_send(command)
            .map_err(|_| "Device connection is busy; try again shortly".into())
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        // Do not drain queued writes when the window owner is gone. Dropping the
        // runtime cancels outstanding operations and the event poll. A write
        // already received by the mixer cannot be recalled here.
        let _ = self.shutdown.send(true);
        self.worker.take(); // Detached only until the cancellation is observed.
    }
}

#[derive(Clone)]
struct Device {
    usb: Usb,
}

impl Device {
    /// Connect and read the state; also returns the mixer's USB serial.
    async fn connect() -> Result<(Self, String, Value), String> {
        let (usb, mixer) = Usb::connect().await?;
        let device = Self { usb };
        let state = device
            .request("GET", "api/v1/state", None)
            .await
            .map_err(|e| e.message())?;
        Ok((device, mixer, state))
    }

    async fn preset(&self, action: PresetAction) -> Result<PresetReply, RequestError> {
        match action {
            PresetAction::List => Ok(PresetReply::List(preset_list(
                self.request("GET", "api/v1/presets", None).await?,
            )?)),
            PresetAction::Create {
                name,
                expected_revision,
            } => {
                let name = name.trim();
                if name.is_empty() || name.len() > 128 {
                    return Err(RequestError::Protocol);
                }
                let value = self
                    .request(
                        "POST",
                        "api/v1/presets",
                        Some(json!({"name":name,"expectedRevision":expected_revision})),
                    )
                    .await?;
                Ok(PresetReply::Created(preset_summary(value)?))
            }
            PresetAction::Apply {
                id,
                expected_revision,
            } => {
                let path = preset_path(&id, true)?;
                Ok(PresetReply::Applied(
                    self.request(
                        "POST",
                        &path,
                        Some(json!({"expectedRevision":expected_revision})),
                    )
                    .await?,
                ))
            }
            PresetAction::Delete { id } => {
                let path = preset_path(&id, false)?;
                let value = self.request("DELETE", &path, None).await?;
                if value != json!({"deleted":id}) {
                    return Err(RequestError::Protocol);
                }
                Ok(PresetReply::Deleted(id))
            }
        }
    }

    /// A lifecycle change the mixer accepts with 202, returning its state.
    async fn transition(
        &self,
        method: &str,
        path: &str,
        body: impl serde::Serialize,
    ) -> Result<rc2fx_core::DeviceState, RequestError> {
        let body = serde_json::to_value(body).map_err(|_| RequestError::Protocol)?;
        let value = self
            .request_with_status(method, path, Some(body), Some(202))
            .await?;
        rc2fx_core::DeviceState::from_json(&value.to_string()).map_err(|_| RequestError::Protocol)
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, RequestError> {
        self.request_with_status(method, path, body, None).await
    }

    async fn request_with_status(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        expected_status: Option<u16>,
    ) -> Result<Value, RequestError> {
        let reply = self.usb.request(method, path, body).await?;
        if !(200..300).contains(&reply.status) {
            return Err(RequestError::Status(
                reply.status,
                reply
                    .body
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ));
        }
        if expected_status.is_some_and(|s| s != reply.status) {
            return Err(RequestError::Protocol);
        }
        Ok(reply.body)
    }
}

#[derive(Debug)]
enum RequestError {
    Usb(String),
    Status(u16, Option<String>),
    TooLarge,
    Protocol,
}

impl RequestError {
    fn message(&self) -> String {
        match self {
            Self::Usb(error) => error.clone(),
            Self::Status(409, Some(detail)) if detail == "revision conflict" => {
                "The mixer's settings changed. Reload them before applying this draft.".into()
            }
            Self::Status(_, Some(detail)) => detail.clone(),
            Self::Status(code, None) => format!("The mixer refused the request ({code})"),
            Self::TooLarge => "The message is too large for the mixer".into(),
            Self::Protocol => "The mixer sent an invalid control message".into(),
        }
    }
}

async fn emit(sender: &mpsc::Sender<Event>, generation: u64, kind: EventKind) -> bool {
    sender.send(Event { generation, kind }).await.is_ok()
}

fn no_mixer() -> RequestError {
    RequestError::Usb("No connected mixer".into())
}

async fn run(mut commands: mpsc::Receiver<Command>, events: mpsc::Sender<Event>) {
    let mut current: Option<Device> = None;
    let mut generation = 0;
    let mut stream_task: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(command) = commands.recv().await {
        match command {
            Command::Selection {
                generation: requested,
                request,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => device.transition("PUT", "api/v1/selection", request).await,
                    None => Err(no_mixer()),
                };
                emit(&events, generation, EventKind::Selection(result.map_err(|error| {
                    if matches!(error, RequestError::Status(409, _)) {
                        "The mixer state changed or a transition is already pending. The path change was not retried.".into()
                    } else { error.message() }
                }))).await;
            }
            Command::RestartEngine {
                generation: requested,
                request,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => {
                        device
                            .transition("POST", "api/v1/engine/restart", request)
                            .await
                    }
                    None => Err(no_mixer()),
                };
                emit(&events, generation, EventKind::RestartEngine(result.map_err(|error| {
                    if matches!(error, RequestError::Status(409, _)) {
                        "The engine or settings changed, or recovery is not ready. Checking the mixer state; no restart was retried.".into()
                    } else { error.message() }
                }))).await;
            }
            Command::Diagnostics {
                generation: requested,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => device.request("GET", "api/v1/diagnostics", None).await,
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::Diagnostics(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::ResetDiagnostics {
                generation: requested,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => {
                        device
                            .request("POST", "api/v1/diagnostics/reset", None)
                            .await
                    }
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::ResetDiagnostics(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::DiagnosticsEvents {
                generation: requested,
                after,
            } if requested == generation => {
                let path = format!("api/v1/diagnostics/events?after={after}");
                let result = match &current {
                    Some(device) => device.request("GET", &path, None).await.and_then(|value| {
                        serde_json::from_value(value)
                            .map_err(|error| RequestError::Status(500, Some(error.to_string())))
                    }),
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::DiagnosticsEvents(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::Settings {
                generation: requested,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => device
                        .request("GET", "api/v1/settings", None)
                        .await
                        .map(|value| value.to_string()),
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::Settings(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::Inputs {
                generation: requested,
            } if requested == generation => {
                let result =
                    match &current {
                        Some(device) => device
                            .request("GET", "api/v1/inputs", None)
                            .await
                            .and_then(|value| {
                                let inventory: rc2fx_core::StockInventory =
                                    serde_json::from_value(value)
                                        .map_err(|_| RequestError::Protocol)?;
                                inventory.validate().map_err(|_| RequestError::Protocol)?;
                                Ok(inventory)
                            }),
                        None => Err(no_mixer()),
                    };
                emit(
                    &events,
                    generation,
                    EventKind::Inputs(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::Routing {
                generation: requested,
                request,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => device.transition("PUT", "api/v1/routing", request).await,
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::Routing(result.map_err(|e| e.message())),
                )
                .await;
            }
            Command::Preset {
                generation: requested,
                action,
            } if requested == generation => {
                let result = match &current {
                    Some(device) => device.preset(action).await,
                    None => Err(no_mixer()),
                };
                emit(
                    &events,
                    generation,
                    EventKind::Preset(result.map_err(|error| PresetError {
                        conflict: matches!(&error, RequestError::Status(409, _)),
                        message: error.message(),
                    })),
                )
                .await;
            }
            Command::Connect { generation: next } => {
                if let Some(task) = stream_task.take() {
                    task.abort();
                }
                current = None;
                generation = next;
                match Device::connect().await {
                    Ok((device, mixer, state)) => {
                        let kind = EventKind::Connected { mixer, state };
                        if !emit(&events, generation, kind).await {
                            break;
                        }
                        stream_task = Some(tokio::spawn(stream_events(
                            device.clone(),
                            generation,
                            events.clone(),
                        )));
                        current = Some(device);
                    }
                    Err(message) => {
                        emit(&events, generation, EventKind::Failure(message)).await;
                    }
                }
            }
            Command::Disconnect { generation: next } => {
                if let Some(task) = stream_task.take() {
                    task.abort();
                }
                current = None;
                generation = next;
            }
            Command::Apply {
                generation: requested,
                update,
            } if requested == generation => {
                if let Some(device) = &current {
                    let result = device
                        .request("PUT", "api/v1/config", Some(apply_body(&update)))
                        .await;
                    let kind = match result {
                        Ok(state) => EventKind::Applied(state),
                        Err(RequestError::Status(409, _)) => EventKind::Conflict,
                        Err(error) => EventKind::Failure(error.message()),
                    };
                    emit(&events, generation, kind).await;
                }
            }
            Command::Refresh {
                generation: requested,
            } if requested == generation => {
                if let Some(device) = &current {
                    let kind = match device.request("GET", "api/v1/state", None).await {
                        Ok(state) => EventKind::Refreshed(state),
                        Err(error) => EventKind::Failure(error.message()),
                    };
                    emit(&events, generation, kind).await;
                }
            }
            _ => {}
        }
    }
    if let Some(task) = stream_task {
        task.abort();
    }
}

/// Poll the mixer's events, backing off while the poll fails. A poll that
/// worked for a while starts the backoff again from one second.
async fn stream_events(device: Device, generation: u64, sender: mpsc::Sender<Event>) {
    let mut delay = 1;
    loop {
        let started = std::time::Instant::now();
        let message = read_stream(&device, generation, &sender)
            .await
            .err()
            .unwrap_or_else(|| "Device event stream closed".into());
        if started.elapsed() > Duration::from_secs(30) {
            delay = 1;
        }
        let kind = EventKind::StreamStatus {
            connected: false,
            message,
        };
        if !emit(&sender, generation, kind).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(15);
    }
}

async fn read_stream(
    device: &Device,
    generation: u64,
    sender: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let mut announced = false;
    loop {
        let value = device
            .request("GET", "api/v1/events", None)
            .await
            .map_err(|e| e.message())?;
        if !announced {
            let kind = EventKind::StreamStatus {
                connected: true,
                message: "Connected".into(),
            };
            if !emit(sender, generation, kind).await {
                return Ok(());
            }
            announced = true;
        }
        if !value.is_null() {
            let name = value
                .get("event")
                .and_then(Value::as_str)
                .ok_or("Invalid USB event")?;
            let data = value.get("data").cloned().ok_or("Invalid USB event data")?;
            let kind = match name {
                "state" => EventKind::State(data),
                "engine" => EventKind::Engine(data),
                "meter" => EventKind::Meter(data),
                "usb" => EventKind::Usb(data),
                _ => return Err("Unknown USB event".into()),
            };
            if !emit(sender, generation, kind).await {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The body of a settings save: serialize the complete typed update.
fn apply_body(update: &rc2fx_core::ConfigUpdate) -> Value {
    serde_json::to_value(update).expect("typed config serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_save_round_trips_the_complete_update() {
        let update = rc2fx_core::ConfigUpdate {
            expected_revision: 7,
            targets: rc2fx_core::Config::default().targets,
        };
        let body = apply_body(&update);
        assert_eq!(body["expectedRevision"], 7);
        assert_eq!(body.as_object().unwrap().len(), 2);
        assert_eq!(
            serde_json::from_value::<rc2fx_core::ConfigUpdate>(body).unwrap(),
            update
        );
    }

    #[test]
    fn preset_ids_and_lists_are_bounded_and_strict() {
        assert_eq!(
            preset_path("preset-1", true).unwrap(),
            "api/v1/presets/preset-1/apply"
        );
        for id in ["../state", "x/apply", "x?token=1", "x%2fy", "", "UPPER"] {
            assert!(preset_path(id, false).is_err(), "{id}");
        }
        assert_eq!(
            preset_list(json!({"presets":[{"id":"preset-1","name":"Voice"}]}))
                .unwrap()
                .len(),
            1
        );
        for invalid in [
            json!({"presets":null}),
            json!({"presets":[],"extra":true}),
            json!({"presets":[{"id":"p","name":"Voice","extra":true}]}),
            json!({"presets":[{"id":"p","name":"Voice"},{"id":"p","name":"Other"}]}),
        ] {
            assert!(preset_list(invalid).is_err());
        }
    }

    #[test]
    fn dropping_owner_stops_worker_without_draining_commands() {
        let mut transport = Transport::new().unwrap();
        // A bounded queue full of commands must not keep the owner alive.
        for generation in 1..=16 {
            let _ = transport.send(Command::Disconnect { generation });
        }
        let worker = transport.worker.take().unwrap();
        drop(transport);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            worker.is_finished(),
            "device worker did not stop after its owner dropped"
        );
        worker.join().unwrap();
    }
}
