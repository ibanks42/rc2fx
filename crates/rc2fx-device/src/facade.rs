//! Persistent, control-only read front door. It owns no Store, DSP, callback or
//! native registration. A snapshot and subscription are captured under one lock.
use crate::diagnostic_history::{
    CallbackDiagnostic, ExportError, FaultObservation, History, process_resources,
};
use crate::events::{Event, EventError, EventKind, Hub, Subscription};
use rc2fx_core::{DeviceState, Meter, PresetSummary};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct Facade(Arc<Mutex<Published>>);
#[derive(Default)]
struct Published {
    state: Option<DeviceState>,
    presets: Vec<PresetSummary>,
    hub: Hub,
    history: History,
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    native_process: Option<crate::diagnostic_history::NativeProcess>,
}
impl Published {
    fn event<T: serde::Serialize>(
        &mut self,
        kind: EventKind,
        value: &T,
    ) -> Option<Result<Event, EventError>> {
        let observed = self.hub.has_clients();
        self.history.event_serialization(observed);
        observed.then(|| Event::serialize(kind, value))
    }
    fn publish_event(&mut self, event: Option<Result<Event, EventError>>) {
        match event {
            Some(Ok(event)) => self.hub.publish(event),
            Some(Err(_)) => self.hub.disconnect_all(),
            None => (),
        }
    }
}
impl Facade {
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn native_process(&self, process: crate::diagnostic_history::NativeProcess) {
        self.0
            .lock()
            .expect("control facade poisoned")
            .native_process = Some(process);
    }
    pub(crate) fn state(&self) -> DeviceState {
        self.0
            .lock()
            .expect("control facade poisoned")
            .state
            .as_ref()
            .expect("controller publishes before handle creation")
            .clone()
    }
    pub(crate) fn presets(&self) -> Vec<PresetSummary> {
        self.0
            .lock()
            .expect("control facade poisoned")
            .presets
            .clone()
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn has_clients(&self) -> bool {
        self.0
            .lock()
            .expect("control facade poisoned")
            .hub
            .has_clients()
    }
    pub(crate) fn subscribe(&self) -> Result<Subscription, EventError> {
        let mut front = self.0.lock().expect("control facade poisoned");
        let state = front.state.as_ref().expect("initialized facade").clone();
        front.hub.subscribe(&state)
    }
    pub(crate) fn publish(&self, state: DeviceState, presets: Vec<PresetSummary>) {
        let mut front = self.0.lock().expect("control facade poisoned");
        front.presets = presets;
        if front.state.as_ref() == Some(&state) {
            return;
        }
        let event = front.event(EventKind::State, &state);
        front.history.state(&state);
        front.state = Some(state);
        front.publish_event(event);
    }
    pub(crate) fn meter(&self, meter: &Meter, callback: &[CallbackDiagnostic]) {
        let mut front = self.0.lock().expect("control facade poisoned");
        front.history.meter(meter, callback);
        let event = front.event(EventKind::Meter, meter);
        front.publish_event(event);
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    /// `shown` goes to subscribers (baselines applied); `raw` is what the
    /// diagnostics export keeps, so an exported report is never relative to a
    /// reset the analysis did not ask for.
    pub(crate) fn native_meter(
        &self,
        meter: Meter,
        raw: Meter,
        native: Vec<rc2fx_native_control::ContextStatus>,
    ) {
        let mut front = self.0.lock().expect("control facade poisoned");
        let event = front.event(EventKind::Meter, &meter);
        front.history.native_meter(raw, native);
        front.publish_event(event);
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn rejection(
        &self,
        generation: u64,
        revision: u64,
        operation: &str,
        expected_revision: Option<u64>,
        targets: &[String],
        reason: &str,
    ) {
        self.0
            .lock()
            .expect("control facade poisoned")
            .history
            .rejection(
                generation,
                revision,
                operation,
                expected_revision,
                targets,
                reason,
            );
    }
    pub(crate) fn sample_resources(&self) {
        let ticket = self
            .0
            .lock()
            .expect("control facade poisoned")
            .history
            .begin_sample(std::time::Instant::now());
        if let Some(ticket) = ticket {
            // Bounded /proc I/O is outside the facade lock and every callback.
            let resources = process_resources();
            let usb = crate::usb::observe();
            #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
            let native_process = {
                self.0
                    .lock()
                    .expect("control facade poisoned")
                    .native_process
                    .clone()
            };
            #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
            let native_resources = native_process.map(|process| process.resources());
            let mut front = self.0.lock().expect("control facade poisoned");
            #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
            front
                .history
                .finish_sample_with_native(ticket, resources, usb, native_resources);
            #[cfg(not(all(feature = "native-pipeline", target_os = "linux")))]
            front.history.finish_sample(ticket, resources, usb);
            let event = front.event(EventKind::Usb, &usb);
            front.publish_event(event);
        }
    }
    pub(crate) fn diagnostic_report(&self) -> Result<Vec<u8>, ExportError> {
        // Copy only bounded Arc metadata while holding the read facade lock.
        let report = self
            .0
            .lock()
            .expect("control facade poisoned")
            .history
            .snapshot();
        report.encode()
    }
    pub(crate) fn fault(
        &self,
        generation: u64,
        published_revision: u64,
        applied_revision: u64,
        message: &str,
        targets: &[FaultObservation],
    ) {
        self.0
            .lock()
            .expect("control facade poisoned")
            .history
            .fault(
                generation,
                published_revision,
                applied_revision,
                message,
                targets,
            );
    }
    pub(crate) fn disconnect(&self) {
        self.0
            .lock()
            .expect("control facade poisoned")
            .hub
            .disconnect_all();
    }
}

// The store needs Unix directory durability and file locking.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Counted<'a>(&'a Cell<u64>);
    impl serde::Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            serializer.serialize_u8(1)
        }
    }
    fn state() -> DeviceState {
        let directory = tempfile::tempdir().unwrap();
        crate::controller::Controller::new(
            crate::store::Store::open(directory.path(), rc2fx_core::Config::default()).unwrap(),
        )
        .state()
    }
    #[tokio::test]
    async fn no_subscriber_skips_serialization_and_reconnect_receives_only_new_observations() {
        let facade = Facade::default();
        facade.publish(state(), vec![]);
        let count = Cell::new(0);
        for _ in 0..100 {
            let mut front = facade.0.lock().unwrap();
            assert!(front.event(EventKind::Meter, &Counted(&count)).is_none());
            assert!(front.event(EventKind::Usb, &Counted(&count)).is_none());
        }
        assert_eq!(count.get(), 0);
        let mut sub = facade.subscribe().unwrap();
        assert_eq!(sub.next().await.unwrap().kind, EventKind::State);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), sub.next())
                .await
                .is_err()
        );
        {
            let mut front = facade.0.lock().unwrap();
            let event = front.event(EventKind::Meter, &Counted(&count));
            front.publish_event(event);
        }
        assert_eq!(sub.next().await.unwrap().kind, EventKind::Meter);
        assert_eq!(count.get(), 1);
        drop(sub);
        {
            let mut front = facade.0.lock().unwrap();
            assert!(front.event(EventKind::Meter, &Counted(&count)).is_none());
        }
        let mut reconnect = facade.subscribe().unwrap();
        assert_eq!(reconnect.next().await.unwrap().kind, EventKind::State);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), reconnect.next())
                .await
                .is_err()
        );
        assert_eq!(count.get(), 1);
        let report: serde_json::Value =
            serde_json::from_slice(&facade.diagnostic_report().unwrap()).unwrap();
        assert_eq!(report["report"]["retention"]["eventSerializations"], 1);
        assert_eq!(
            report["report"]["retention"]["skippedUnobservedEventSerializations"],
            202
        );
    }
}
