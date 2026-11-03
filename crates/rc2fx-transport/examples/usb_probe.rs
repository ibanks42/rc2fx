//! End-to-end USB check. Read-only unless --write-roundtrip is explicitly used;
//! that flag reapplies the exact current targets with optimistic revision checking.
use rc2fx_transport::{Command, EventKind, PresetAction, Transport};
use std::time::{Duration, Instant};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let write_roundtrip = args.iter().any(|a| a == "--write-roundtrip");
    let extended = args.iter().any(|a| a == "--extended");
    let seconds = args
        .windows(2)
        .find(|a| a[0] == "--seconds")
        .map(|a| a[1].parse::<u64>().map_err(|_| "Invalid --seconds"))
        .transpose()?
        .unwrap_or(10)
        .clamp(10, 3600);
    let mut transport = Transport::new()?;
    transport.send(Command::Connect { generation: 1 })?;
    let deadline = Instant::now() + Duration::from_secs(70);
    let mut connected = false;
    let mut stream = false;
    let mut state_events = 0;
    let mut meter_events = 0;
    let mut usb_events = 0;
    let mut applied = !write_roundtrip;
    let mut expected_targets = None;
    let mut reads = 0;
    let mut first_meter = None;
    let mut last_meter = None;
    let mut finish = deadline;
    while Instant::now() < finish {
        while let Some(event) = transport.try_recv() {
            match event.kind {
                EventKind::Connected { state, .. } => {
                    println!(
                        "USB connected; settings revision {}",
                        state["config"]["revision"]
                    );
                    connected = true;
                    finish = Instant::now() + Duration::from_secs(seconds);
                    if write_roundtrip {
                        let targets = state["config"]["targets"].clone();
                        expected_targets = Some(targets.clone());
                        transport.send(Command::Apply {
                            generation: 1,
                            update: rc2fx_core::ConfigUpdate {
                                expected_revision: state["config"]["revision"]
                                    .as_u64()
                                    .ok_or("Missing revision")?,
                                targets: serde_json::from_value(targets)
                                    .map_err(|e| e.to_string())?,
                            },
                        })?;
                    }
                    if extended {
                        transport.send(Command::Inputs { generation: 1 })?;
                        transport.send(Command::Preset {
                            generation: 1,
                            action: PresetAction::List,
                        })?;
                        transport.send(Command::Diagnostics { generation: 1 })?;
                    }
                }
                EventKind::StreamStatus { connected, message } => {
                    println!("USB stream: {message}");
                    stream = connected;
                }
                EventKind::State(_) => state_events += 1,
                EventKind::Meter(meter) => {
                    meter_events += 1;
                    if first_meter.is_none() {
                        first_meter = Some(meter.clone());
                    }
                    last_meter = Some(meter);
                }
                EventKind::Usb(_) => usb_events += 1,
                EventKind::Applied(state) => {
                    if expected_targets.as_ref() != Some(&state["config"]["targets"]) {
                        return Err("Applied targets differ from the unchanged input".into());
                    }
                    println!(
                        "Unchanged settings round trip applied; revision {}",
                        state["config"]["revision"]
                    );
                    applied = true;
                }
                EventKind::Conflict => {
                    return Err("Settings changed concurrently; no write retried".into());
                }
                EventKind::Inputs(result) => {
                    result?;
                    reads += 1;
                    println!("USB input inventory received");
                }
                EventKind::Preset(result) => {
                    result.map_err(|e| e.message)?;
                    reads += 1;
                    println!("USB preset inventory received");
                }
                EventKind::Diagnostics(result) => {
                    let report = result?;
                    reads += 1;
                    println!(
                        "USB diagnostics received ({} bytes)",
                        serde_json::to_vec(&report).unwrap().len()
                    );
                }
                EventKind::Failure(error) => return Err(error),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "state events: {state_events}; meter events: {meter_events}; USB observations: {usb_events}"
    );
    if !connected || !stream || state_events == 0 || !applied || (extended && reads != 3) {
        return Err("USB state/event round trip did not complete".into());
    }
    if let (Some(first), Some(last)) = (first_meter, last_meter) {
        println!("First meter: {first}");
        println!("Last meter: {last}");
    }
    Ok(())
}
