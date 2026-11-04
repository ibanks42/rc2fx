//! The control API, served over USB 1 (see `usb_control`). Requests name a
//! method and an `api/v1/...` path with an optional JSON body; replies carry a
//! status code and a JSON body. This is not an audio runtime.
use crate::{
    coordinator::{Error, Handle, receive},
    events::{EventError, Subscription},
    store::StoreError,
};
use rc2fx_usb::Request;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an event poll waits for the next event.
const EVENT_WAIT: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
pub struct Options {
    /// Read-only local startup setting; inaccessible through API writes.
    pub stock_config: Option<PathBuf>,
    pub catalog: Vec<crate::stock_inputs::CatalogChannel<'static>>,
    /// What `GET api/v1/version` reports, read once at startup.
    pub versions: rc2fx_core::DeviceVersions,
}

/// A response body, carried as validated JSON text rather than a `Value`
/// tree: typed replies are serialized straight to it, and payloads the
/// controller already serialized (events, the diagnostics export) are
/// validated once and then passed through as they are. The wire bytes are the
/// same either way — the host still decodes `Reply` with a `Value` body.
pub type Reply = rc2fx_usb::Reply<Box<RawValue>>;

fn reply(status: u16, value: &impl Serialize) -> Reply {
    // `to_raw_value` is serde_json's own serializer: it writes the JSON text
    // once and hands it back as a RawValue, so a typed reply is never
    // serialized and then parsed again.
    match serde_json::value::to_raw_value(value) {
        Ok(body) => Reply { status, body },
        Err(_) => error(500, "Response serialization failed"),
    }
}

/// A reply from bytes the controller already serialized: validated once here
/// (this is the boundary; nothing trusts the bytes before it) and then carried
/// as raw JSON, without building a value tree.
fn reply_bytes(status: u16, data: &[u8]) -> Reply {
    match serde_json::from_slice::<&RawValue>(data) {
        Ok(body) => Reply {
            status,
            body: body.to_owned(),
        },
        Err(_) => error(500, "Invalid response JSON"),
    }
}

/// The event envelope `{"event": <kind>, "data": <payload>}`, built without a
/// value tree: the payload bytes the controller serialized are validated once
/// here (the trust boundary) and written straight into the envelope. Returns
/// None when the payload is not valid JSON, which the caller answers as 500
/// exactly as before.
pub(crate) fn event_reply(kind: &str, data: &[u8]) -> Option<Reply> {
    let text = std::str::from_utf8(data).ok()?;
    // The one validation of the incoming payload: borrowed, so no copy either.
    let payload: &RawValue = serde_json::from_str(text).ok()?;
    // `to_raw_value` serializes the envelope around that already-validated
    // text without scanning it a second time; it is a serde serializer, not
    // unchecked construction from arbitrary bytes.
    let body = serde_json::value::to_raw_value(&RawEvent {
        event: kind,
        data: payload,
    })
    .ok()?;
    Some(Reply { status: 200, body })
}

#[derive(Serialize)]
struct RawEvent<'a> {
    event: &'a str,
    data: &'a RawValue,
}

/// The empty poll reply: JSON `null`, exactly as before.
fn empty() -> Reply {
    let body = RawValue::from_string("null".to_owned()).expect("null is JSON");
    Reply { status: 200, body }
}

/// An error page, for the USB layer's own failures (a bad request, a reply
/// that would not fit): the same shape the API's errors have.
#[cfg(target_os = "linux")]
pub(crate) fn failure(status: u16, message: impl AsRef<str>) -> Reply {
    error(status, message)
}

fn error(status: u16, message: impl AsRef<str>) -> Reply {
    reply(status, &json!({"error": message.as_ref()}))
}

fn operation_error(error_value: Error) -> Reply {
    let status = match &error_value {
        Error::Store(StoreError::Invalid(_)) => 400,
        Error::Store(StoreError::Conflict) | Error::Audio(_) | Error::RuntimeFault => 409,
        Error::NativeUnsupported(_) | Error::SuppressionInUse => 422,
        Error::Store(StoreError::NotFound) => 404,
        Error::Busy
        | Error::Stopped
        | Error::Native(_)
        | Error::NativeBusy
        | Error::Events(EventError::TooManyClients) => 503,
        _ => 500,
    };
    error(status, error_value.to_string())
}

/// One `key=value` from a request path's query string.
fn query(path: &str, key: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

fn parse<T: serde::de::DeserializeOwned>(body: Option<Value>) -> Result<T, Reply> {
    let value = body.ok_or_else(|| error(400, "A JSON body is required"))?;
    serde_json::from_value(value).map_err(|e| error(400, e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreatePreset {
    name: String,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Revision {
    expected_revision: u64,
}

/// One client's view of the API, with its event subscription.
pub(crate) struct Api {
    controller: Handle,
    options: Arc<Options>,
    // Held by the blocking task itself, including after the request ends.
    discovery_permit: Arc<Semaphore>,
    diagnostics_permit: Arc<Semaphore>,
    subscription: Option<Subscription>,
}

impl Api {
    pub(crate) fn new(controller: Handle, options: Options) -> Self {
        Self {
            controller,
            options: Arc::new(options),
            discovery_permit: Arc::new(Semaphore::new(1)),
            diagnostics_permit: Arc::new(Semaphore::new(1)),
            subscription: None,
        }
    }

    /// A new client: forget the old one's event subscription.
    pub(crate) fn reset(&mut self) {
        self.subscription = None;
    }

    pub(crate) async fn request(&mut self, input: Request) -> Reply {
        let Request { method, path, body } = input;
        if method == "GET" && path == "api/v1/events" && body.is_none() {
            return self.next_event().await;
        }
        let controller = &self.controller;
        match (method.as_str(), path.as_str()) {
            ("GET", "api/v1/diagnostics") => self.diagnostics().await,
            // The diagnostics event log, one page at a time.
            ("GET", path) if path.starts_with("api/v1/diagnostics/events") => {
                let after = query(path, "after")
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                match receive(controller.diagnostics_events(after, rc2fx_core::EVENTS_PAGE)).await {
                    Ok(events) => reply(200, &events),
                    Err(error) => operation_error(error),
                }
            }
            // Restart what the window shows: baselines and the event log.
            ("POST", "api/v1/diagnostics/reset") => {
                match receive(controller.reset_diagnostics()).await {
                    Ok(state) => reply(200, &state),
                    Err(error) => operation_error(error),
                }
            }
            ("GET", "api/v1/inputs") => self.inputs().await,
            // Installed RC2FX release and validated RØDE firmware. Separate
            // from the state so DeviceState's strict schema is unchanged.
            ("GET", "api/v1/version") => reply(200, &self.options.versions),
            // The saved settings file, for the installer to carry across a reflash.
            ("GET", "api/v1/settings") => match receive(controller.settings()).await {
                Ok(data) => reply_bytes(200, &data),
                Err(error) => operation_error(error),
            },
            ("GET", "api/v1/state") => match receive(controller.state()).await {
                Ok(state) => reply(200, &state),
                Err(error) => operation_error(error),
            },
            ("PUT", "api/v1/config") => match parse::<rc2fx_core::ConfigUpdate>(body) {
                Ok(update) => match receive(controller.apply(update)).await {
                    Ok(state) => reply(200, &state),
                    Err(error) => operation_error(error),
                },
                Err(reply) => reply,
            },
            ("PUT", "api/v1/routing") => match parse::<rc2fx_core::RoutingRequest>(body) {
                Ok(request) => match receive(controller.routing(request)).await {
                    Ok(state) => reply(202, &state),
                    Err(error) => operation_error(error),
                },
                Err(reply) => reply,
            },
            ("PUT", "api/v1/selection") => match parse::<rc2fx_core::SelectionRequest>(body) {
                Ok(request) => match receive(controller.selection(request)).await {
                    Ok(state) => reply(202, &state),
                    Err(error) => operation_error(error),
                },
                Err(reply) => reply,
            },
            ("POST", "api/v1/engine/restart") => {
                match parse::<rc2fx_core::EngineRestartRequest>(body) {
                    Ok(request) => match receive(controller.restart(request)).await {
                        Ok(state) => reply(202, &state),
                        Err(error) => operation_error(error),
                    },
                    Err(reply) => reply,
                }
            }
            ("GET", "api/v1/presets") => match receive(controller.presets()).await {
                Ok(presets) => reply(200, &json!({"presets": presets})),
                Err(error) => operation_error(error),
            },
            ("POST", "api/v1/presets") => match parse::<CreatePreset>(body) {
                Ok(input) => {
                    let created = controller.create_preset(input.name, input.expected_revision);
                    match receive(created).await {
                        Ok(preset) => reply(201, &preset),
                        Err(error) => operation_error(error),
                    }
                }
                Err(reply) => reply,
            },
            ("POST", _) if path.starts_with("api/v1/presets/") && path.ends_with("/apply") => {
                let id = &path["api/v1/presets/".len()..path.len() - "/apply".len()];
                if !rc2fx_core::valid_id(id) {
                    return error(404, "Unknown preset");
                }
                match parse::<Revision>(body) {
                    Ok(input) => {
                        let applied =
                            controller.apply_preset(id.to_owned(), input.expected_revision);
                        match receive(applied).await {
                            Ok(state) => reply(200, &state),
                            Err(error) => operation_error(error),
                        }
                    }
                    Err(reply) => reply,
                }
            }
            ("DELETE", _) if path.starts_with("api/v1/presets/") => {
                let id = path["api/v1/presets/".len()..].to_owned();
                if !rc2fx_core::valid_id(&id) {
                    return error(404, "Unknown preset");
                }
                match receive(controller.delete_preset(id.clone())).await {
                    Ok(()) => reply(200, &json!({"deleted": id})),
                    Err(error) => operation_error(error),
                }
            }
            _ => error(404, "Unknown endpoint"),
        }
    }

    /// The next state, engine, meter or USB event, or null after a short wait.
    async fn next_event(&mut self) -> Reply {
        if self.subscription.is_none() {
            match receive(self.controller.subscribe()).await {
                Ok(subscription) => self.subscription = Some(subscription),
                Err(error) => return operation_error(error),
            }
        }
        let subscription = self.subscription.as_mut().expect("subscribed above");
        match tokio::time::timeout(EVENT_WAIT, subscription.next()).await {
            Ok(Some(event)) => event_reply(event.kind.name(), &event.data)
                .unwrap_or_else(|| error(500, "Invalid event data")),
            Ok(None) => {
                self.subscription = None;
                error(503, "Event subscription closed")
            }
            Err(_) => empty(),
        }
    }

    async fn diagnostics(&self) -> Reply {
        let Ok(permit) = self.diagnostics_permit.clone().try_acquire_owned() else {
            return error(503, "Diagnostics export is busy; retry shortly");
        };
        let controller = self.controller.clone();
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            controller.diagnostic_report()
        });
        match tokio::time::timeout(Duration::from_secs(2), work).await {
            Ok(Ok(Ok(data))) => reply_bytes(200, &data),
            Ok(Ok(Err(error))) => operation_error(error),
            _ => error(503, "Diagnostics export did not complete"),
        }
    }

    async fn inputs(&self) -> Reply {
        let Some(path) = self.options.stock_config.clone() else {
            return reply(
                200,
                &crate::stock_inputs::unavailable("Show discovery is not configured on this mixer"),
            );
        };
        // A read that is still running keeps the slot; queue behind it, but
        // within one deadline for the whole request.
        let deadline = tokio::time::Instant::now() + DISCOVERY_TIMEOUT;
        let permit =
            match tokio::time::timeout_at(deadline, self.discovery_permit.clone().acquire_owned())
                .await
            {
                Ok(Ok(permit)) if tokio::time::Instant::now() < deadline => permit,
                _ => return error(503, "Show discovery is busy; retry shortly"),
            };
        let config = match tokio::time::timeout_at(deadline, receive(self.controller.state())).await
        {
            Ok(Ok(state)) => state.config,
            Ok(Err(error)) => return operation_error(error),
            Err(_) => return error(503, "Show discovery did not complete"),
        };
        let options = self.options.clone();
        read_inventory(deadline, permit, move || {
            crate::stock_inputs::read(&path, &config, &options.catalog)
        })
        .await
    }
}

async fn read_inventory(
    deadline: tokio::time::Instant,
    permit: tokio::sync::OwnedSemaphorePermit,
    read: impl FnOnce() -> rc2fx_core::StockInventory + Send + 'static,
) -> Reply {
    let unreadable = || {
        reply(
            200,
            &crate::stock_inputs::unavailable("The saved show could not be read; retry shortly"),
        )
    };
    if tokio::time::Instant::now() >= deadline {
        return unreadable();
    }
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read()
    });
    match tokio::time::timeout_at(deadline, work).await {
        Ok(Ok(inventory)) if tokio::time::Instant::now() < deadline => reply(200, &inventory),
        _ => unreadable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{controller::Controller, coordinator::Coordinator, store::Store};
    use rc2fx_core::{Config, ConfigUpdate, DeviceState};

    #[test]
    fn native_runtime_rejections_are_not_revision_conflicts() {
        assert_eq!(
            operation_error(Error::Store(StoreError::Conflict)).status,
            409
        );
        assert_eq!(
            operation_error(Error::NativeUnsupported("structural change".into())).status,
            422
        );
        assert_eq!(operation_error(Error::NativeBusy).status, 503);
        assert_eq!(
            operation_error(Error::Native("native unavailable".into())).status,
            503
        );
    }

    /// The diagnostics endpoints: a reset answers with the state, and the event
    /// log answers with a well-formed page a client can page through.
    /// The reply body as the host decodes it (a `Value`): the wire bytes are
    /// unchanged, only the device no longer builds one on the way out.
    fn body(reply: &Reply) -> Value {
        serde_json::from_str(reply.body.get()).unwrap()
    }
    fn decode<T: serde::de::DeserializeOwned>(reply: &Reply) -> T {
        serde_json::from_str(reply.body.get()).unwrap()
    }

    /// The new bridge must mean exactly the same thing on the wire as the old
    /// Value roundtrip: both decode, with the HOST's `Reply`, to the same
    /// value. (Byte identity is not the guarantee — the Value path sorts keys
    /// and normalises numbers; the wire *contract* is the decoded document.)
    #[test]
    fn the_raw_bridge_matches_the_former_value_bridge_semantically() {
        // An event payload as the hub serializes it, with awkward content:
        // quotes, backslashes, a newline, non-ASCII and a nested array.
        let payload = br#"{"id":"mic-1","voice":"a \"quoted\" \\ path","note":"line\nbreak \u00e9","spectrum":[0.5,1e-3,-0.0]}"#;
        let raw = event_reply("meter", payload).expect("valid payload");
        // The old path: parse to Value, wrap with json!, to_value, serialize.
        let data: Value = serde_json::from_slice(payload).unwrap();
        let old = serde_json::to_vec(&json!({
            "status": 200,
            "body": {"event": "meter", "data": data}
        }))
        .unwrap();
        let new = serde_json::to_vec(&raw).unwrap();
        // Byte identity is not possible (the Value path sorts keys and
        // normalises numbers); the HOST's decode must be equal, which is what
        // the wire contract is.
        let decoded: rc2fx_usb::Reply = serde_json::from_slice(&new).unwrap();
        let before: rc2fx_usb::Reply = serde_json::from_slice(&old).unwrap();
        assert_eq!(decoded.status, before.status);
        assert_eq!(decoded.body, before.body);
        // The raw path keeps the payload's own text: the escaped backslash and
        // the \u escape are written exactly as the controller serialized them.
        let text = String::from_utf8(new.clone()).unwrap();
        assert!(text.contains(r#""a \"quoted\" \\ path""#), "{text}");
        assert!(text.contains(r#"\u00e9"#), "{text}");
        assert!(text.contains(r#""line\nbreak"#), "{text}");
        // An ordinary typed reply (the state page) decodes identically too.
        let directory = tempfile::tempdir().unwrap();
        let state = crate::controller::Controller::new(
            crate::store::Store::open(directory.path(), rc2fx_core::Config::default()).unwrap(),
        )
        .state();
        let typed = reply(200, &state);
        let old = serde_json::to_vec(&json!({
            "status": 200,
            "body": serde_json::to_value(&state).unwrap()
        }))
        .unwrap();
        let decoded: rc2fx_usb::Reply =
            serde_json::from_slice(&serde_json::to_vec(&typed).unwrap()).unwrap();
        let before: rc2fx_usb::Reply = serde_json::from_slice(&old).unwrap();
        assert_eq!(decoded.body, before.body);
        // Errors and the empty poll reply keep their exact shapes.
        let failure = error(500, "Invalid event data");
        assert_eq!(
            serde_json::to_vec(&failure).unwrap(),
            br#"{"status":500,"body":{"error":"Invalid event data"}}"#.to_vec()
        );
        assert_eq!(
            serde_json::to_vec(&empty()).unwrap(),
            br#"{"status":200,"body":null}"#.to_vec()
        );
    }

    /// `shown` is relative and the raw meter keeps its cumulative figures:
    /// the export and the window must not see each other's numbers.
    #[test]
    fn reset_relative_and_raw_meters_stay_independent() {
        use rc2fx_core::{Meter, Performance, TargetMeter, Timing};
        let timing = Timing {
            calls: 10,
            total_ns: 1_000,
            max_ns: 500,
            ..Timing::default()
        };
        let mut log = crate::diagnostics_log::DiagnosticsLog::default();
        let meter = || Meter {
            message_type: rc2fx_core::MeterType::Meter,
            revision: 1,
            generation: 1,
            synthetic: false,
            diagnostics_epoch: 0,
            targets: vec![TargetMeter {
                id: "mic-1".into(),
                callbacks: 10,
                processed_frames: 100,
                latency_samples: 920,
                performance: Some(Performance {
                    callback_budget_ns: 2_333_333,
                    worker_budget_ns: 9_333_333,
                    xruns: 2,
                    callback: timing.clone(),
                    worker_wall: timing.clone(),
                    worker_cpu: timing.clone(),
                    helper_late: Some(1),
                    helper_stalls: Some(0),
                }),
                ..TargetMeter {
                    id: "mic-1".into(),
                    input_peak: 0.,
                    output_peak: 0.,
                    rms: 0.,
                    gain_reduction: 0.,
                    voice: 0.,
                    gate_open: 0.,
                    clipped: 0,
                    spectrum: vec![],
                    callbacks: 0,
                    processed_frames: 0,
                    suppression_frames: 0,
                    missed_deadlines: 0,
                    stale_results: 0,
                    input_drops: 0,
                    output_drops: 0,
                    latency_samples: 0,
                    worker_failed: false,
                    performance: None,
                }
            }],
        };
        // First reading is the baseline; then a reset; then growth.
        let first = meter();
        log.observe(Some(&first), &[], 1_000);
        log.reset(1_100);
        let mut raw = meter();
        raw.targets[0].callbacks = 14;
        log.observe(Some(&raw), &[], 1_200);
        // The reading itself was rewritten in place (that is its owner's copy),
        // but `shown()` must not touch a meter handed to it.
        let mut cumulative = meter();
        cumulative.targets[0].callbacks = 20;
        let before = cumulative.clone();
        let shown = log.shown(&cumulative);
        assert_eq!(cumulative, before, "shown() must not modify its input");
        assert_eq!(cumulative.targets[0].callbacks, 20, "raw stays cumulative");
        assert_eq!(
            shown.targets[0].callbacks, 10,
            "shown is relative (20 - 10)"
        );
        assert_eq!(shown.targets[0].performance.as_ref().unwrap().xruns, 0);
        assert_eq!(shown.diagnostics_epoch, 1);
        assert_eq!(cumulative.diagnostics_epoch, 0);
    }

    /// Host-only measurement of the production bridge against the former one,
    /// on the real 19-target fixture. Not an ARM speedup claim.
    /// `cargo test -p rc2fx-device --features native-pipeline --release \
    ///   bench_the_event_bridge -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_the_event_bridge() {
        // A diagnostics export with retained meter telemetry, outside the repo.
        let Ok(path) = std::env::var("RC2FX_BENCH_FIXTURE") else {
            eprintln!("set RC2FX_BENCH_FIXTURE to a diagnostics export; nothing to measure");
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("fixture {path} is absent; nothing to measure");
            return;
        };
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        let meter = document["samples"]
            .as_array()
            .and_then(|samples| {
                samples
                    .iter()
                    .rev()
                    .find_map(|sample| sample.get("telemetry"))
            })
            .and_then(|telemetry| telemetry.get("meter"))
            .expect("a retained meter in the fixture");
        let payload = serde_json::to_vec(meter).unwrap();
        let bench = |name: &str, mut run: Box<dyn FnMut() -> usize>| {
            for _ in 0..100 {
                std::hint::black_box(run());
            }
            let mut times = Vec::new();
            for _ in 0..7 {
                let start = std::time::Instant::now();
                let mut count = 0u32;
                while start.elapsed() < std::time::Duration::from_millis(100) {
                    std::hint::black_box(run());
                    count += 1;
                }
                times.push(start.elapsed().as_secs_f64() * 1e6 / f64::from(count));
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{name}: median_us={:.2} min_us={:.2} max_us={:.2}",
                times[3], times[0], times[6]
            );
        };
        // The former path: payload bytes → Value, json! wrapper, to_value,
        // serialize the envelope, pack.
        bench(
            "event bridge (Value roundtrip) + pack",
            Box::new(|| {
                let data: Value = serde_json::from_slice(std::hint::black_box(&payload)).unwrap();
                let body = serde_json::to_value(json!({"event": "meter", "data": data})).unwrap();
                let envelope = serde_json::to_vec(&rc2fx_usb::Reply { status: 200, body }).unwrap();
                rc2fx_usb::pack(&envelope, rc2fx_usb::MAX_REPLY)
                    .unwrap()
                    .len()
            }),
        );
        // The production bridge: validated raw payload carried through the
        // envelope, no value tree, then the same serialize and pack.
        bench(
            "event bridge (validated raw, production) + pack",
            Box::new(|| {
                let reply = event_reply("meter", std::hint::black_box(&payload)).unwrap();
                let envelope = serde_json::to_vec(&reply).unwrap();
                rc2fx_usb::pack(&envelope, rc2fx_usb::MAX_REPLY)
                    .unwrap()
                    .len()
            }),
        );
        // The export path, for the same reason: bytes → Value → serialize,
        // against bytes → validated raw → serialize.
        let export = serde_json::to_vec(&json!({
            "schema": 1, "samples": document["samples"].clone()
        }))
        .unwrap();
        bench(
            "export reply (Value roundtrip) + pack",
            Box::new(|| {
                let body: Value = serde_json::from_slice(std::hint::black_box(&export)).unwrap();
                let envelope = serde_json::to_vec(&json!({"status": 200, "body": body})).unwrap();
                rc2fx_usb::pack(&envelope, rc2fx_usb::MAX_REPLY)
                    .unwrap()
                    .len()
            }),
        );
        bench(
            "export reply (validated raw, production) + pack",
            Box::new(|| {
                let reply = reply_bytes(200, std::hint::black_box(&export));
                let envelope = serde_json::to_vec(&reply).unwrap();
                rc2fx_usb::pack(&envelope, rc2fx_usb::MAX_REPLY)
                    .unwrap()
                    .len()
            }),
        );
        println!(
            "fixture={path} payload_bytes={} export_bytes={}",
            payload.len(),
            export.len()
        );
    }

    /// Payloads that are not JSON are refused at the boundary (the event path
    /// answers 500 exactly as before), and the export path does the same.
    #[test]
    fn invalid_raw_payloads_are_refused_at_the_boundary() {
        for bad in [
            &b"not json"[..],
            br#"{"a":}"#,
            br#"[1,2"#,
            &[0xff, 0xfe],
            br#"{"a":"unterminated}"#,
        ] {
            assert!(event_reply("meter", bad).is_none(), "{:?}", bad);
            assert_eq!(reply_bytes(200, bad).status, 500);
        }
        // Deeply nested but valid JSON is accepted (serde_json's own limit
        // applies as it did to the Value parse).
        let mut deep = String::new();
        for _ in 0..100 {
            deep.push('[');
        }
        deep.push('1');
        for _ in 0..100 {
            deep.push(']');
        }
        assert!(event_reply("meter", deep.as_bytes()).is_some());
    }

    /// The reply still packs and unpacks through the real framing, within the
    /// existing limits.
    #[test]
    fn a_raw_reply_packs_and_unpacks_within_the_usb_limits() {
        let payload = br#"{"targets":[{"id":"mic-1","callbacks":1}]}"#;
        let reply = event_reply("meter", payload).unwrap();
        let bytes = serde_json::to_vec(&reply).unwrap();
        let packed = rc2fx_usb::pack(&bytes, rc2fx_usb::MAX_REPLY).unwrap();
        let unpacked = rc2fx_usb::unpack(&packed, rc2fx_usb::MAX_REPLY).unwrap();
        assert_eq!(unpacked, bytes);
        let decoded: rc2fx_usb::Reply = serde_json::from_slice(&unpacked).unwrap();
        assert_eq!(decoded.body["data"]["targets"][0]["id"], "mic-1");
        // Over the limit is refused, as before.
        let big = vec![b' '; rc2fx_usb::MAX_REPLY + 1];
        assert!(rc2fx_usb::pack(&big, rc2fx_usb::MAX_REPLY).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn version_reports_what_was_read_and_omits_what_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, owner) = Coordinator::new(
            Controller::new(Store::open(dir.path(), Config::default()).unwrap()),
            None,
        )
        .unwrap();
        let thread = std::thread::spawn(move || owner.run());
        let get = || Request {
            method: "GET".into(),
            path: "api/v1/version".into(),
            body: None,
        };
        let mut unknown = Api::new(handle.clone(), Options::default());
        let reply = unknown.request(get()).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body.get(), "{}");
        let versions = rc2fx_core::DeviceVersions {
            runtime: Some("0.1.0".into()),
            protocol: Some(1),
            firmware: Some("1.7.7".into()),
            firmware_profile: Some("rodecaster-pro-ii-1.7.7".into()),
        };
        let mut known = Api::new(
            handle.clone(),
            Options {
                versions: versions.clone(),
                ..Options::default()
            },
        );
        let reply = known.request(get()).await;
        assert_eq!(reply.status, 200);
        let decoded: rc2fx_core::DeviceVersions = serde_json::from_str(reply.body.get()).unwrap();
        assert_eq!(decoded, versions);
        handle.close();
        handle.shutdown().await.unwrap();
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn diagnostics_reset_and_event_pages_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, owner) = Coordinator::new(
            Controller::new(Store::open(dir.path(), Config::default()).unwrap()),
            None,
        )
        .unwrap();
        let thread = std::thread::spawn(move || owner.run());
        struct Close(crate::coordinator::Handle);
        impl Drop for Close {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(handle.clone());
        let mut api = Api::new(handle.clone(), Options::default());
        let request = |method: &str, path: &str| Request {
            method: method.into(),
            path: path.into(),
            body: None,
        };
        let before = api
            .request(request("GET", "api/v1/diagnostics/events?after=0"))
            .await;
        assert_eq!(before.status, 200);
        let page: rc2fx_core::DiagnosticsEvents = decode(&before);
        assert!(page.next_seq >= 1);
        assert!(!page.more);
        // Reset: 200 with the state, and the log records where it happened.
        let reset = api
            .request(request("POST", "api/v1/diagnostics/reset"))
            .await;
        assert_eq!(reset.status, 200);
        let _: DeviceState = decode(&reset);
        let after = api
            .request(request("GET", "api/v1/diagnostics/events?after=0"))
            .await;
        let page: rc2fx_core::DiagnosticsEvents = decode(&after);
        let last = page.events.last().expect("the reset entry");
        assert_eq!(last.kind, rc2fx_core::DiagnosticsKind::Reset);
        // Asking from there returns nothing more.
        let quiet = api
            .request(request(
                "GET",
                &format!("api/v1/diagnostics/events?after={}", page.next_seq - 1),
            ))
            .await;
        let quiet: rc2fx_core::DiagnosticsEvents = decode(&quiet);
        assert!(quiet.events.is_empty());
        assert_eq!(quiet.lost, 0);
        assert!(quiet.now_ns > 0);
        handle.close();
        handle.shutdown().await.unwrap();
        thread.join().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn settings_presets_conflicts_and_events() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, owner) = Coordinator::new(
            Controller::new(Store::open(dir.path(), Config::default()).unwrap()),
            None,
        )
        .unwrap();
        let thread = std::thread::spawn(move || owner.run());
        struct Close(crate::coordinator::Handle);
        impl Drop for Close {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(handle.clone());
        let mut api = Api::new(handle.clone(), Options::default());
        let request = |method: &str, path: &str, body| Request {
            method: method.into(),
            path: path.into(),
            body,
        };
        let state = api.request(request("GET", "api/v1/state", None)).await;
        assert_eq!(state.status, 200);
        let original: DeviceState = decode(&state);
        // The API enforces the budget even for deselected targets and without
        // an attached audio runtime; a rejection must not persist a partial edit.
        let mut conflicting = original.config.targets.clone();
        conflicting
            .iter_mut()
            .find(|t| t.id == "mic-1")
            .unwrap()
            .suppression
            .enabled = true;
        conflicting
            .iter_mut()
            .find(|t| t.id == "send-chat")
            .unwrap()
            .suppression
            .enabled = true;
        let refused = api
            .request(request(
                "PUT",
                "api/v1/config",
                Some(
                    serde_json::to_value(ConfigUpdate {
                        expected_revision: original.config.revision,
                        targets: conflicting,
                    })
                    .unwrap(),
                ),
            ))
            .await;
        assert_eq!(refused.status, 400);
        let unchanged = api.request(request("GET", "api/v1/state", None)).await;
        assert_eq!(
            body(&unchanged)["config"],
            serde_json::to_value(&original.config).unwrap()
        );
        let first = api.request(request("GET", "api/v1/events", None)).await;
        assert_eq!(body(&first)["event"], "state");
        let mut targets = original.config.targets.clone();
        targets[0].gain.db = -7.0;
        let update = serde_json::to_value(ConfigUpdate {
            expected_revision: original.config.revision,
            targets,
        })
        .unwrap();
        let saved = api
            .request(request("PUT", "api/v1/config", Some(update.clone())))
            .await;
        assert_eq!(saved.status, 200);
        assert_eq!(
            body(&saved)["config"]["revision"],
            original.config.revision + 1
        );
        let stale = api
            .request(request("PUT", "api/v1/config", Some(update)))
            .await;
        assert_eq!(stale.status, 409, "a stale revision is a conflict");
        let event = api.request(request("GET", "api/v1/events", None)).await;
        assert_eq!(
            body(&event)["data"]["config"]["revision"],
            original.config.revision + 1
        );
        let settings = api.request(request("GET", "api/v1/settings", None)).await;
        assert_eq!(settings.status, 200);
        assert!(body(&settings).is_object());
        let revision = original.config.revision + 1;
        let create = json!({"name": "Voice", "expectedRevision": revision});
        let preset = api
            .request(request("POST", "api/v1/presets", Some(create)))
            .await;
        assert_eq!(preset.status, 201);
        let list = api.request(request("GET", "api/v1/presets", None)).await;
        assert_eq!(body(&list)["presets"].as_array().unwrap().len(), 1);
        for (method, path, body) in [
            ("PUT", "api/v1/config", None),
            ("PUT", "api/v1/config", Some(json!({"unknown": true}))),
            ("GET", "api/v1/elsewhere", None),
            ("DELETE", "api/v1/presets/../state", None),
        ] {
            let status = api.request(request(method, path, body)).await.status;
            assert!((400..500).contains(&status), "{method} {path}: {status}");
        }
        api.reset();
        handle.close();
        handle.shutdown().await.unwrap();
        thread.join().unwrap();
        let stored = Store::open(dir.path(), Config::default()).unwrap();
        assert_eq!(stored.config().targets[0].gain.db, -7.0);
    }
}

#[cfg(all(test, unix))]
#[path = "api/inputs_tests.rs"]
mod inputs_tests;
