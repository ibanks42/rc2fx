//! Bounded control-thread notifications for desktop clients.
//! Serialization and publication must never run on a JACK processing thread.
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

pub const MAX_CLIENTS: usize = 4;
pub const QUEUED_EVENTS: usize = 8;
pub const MAX_EVENT_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    State,
    Engine,
    Meter,
    Usb,
}

impl EventKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Engine => "engine",
            Self::Meter => "meter",
            Self::Usb => "usb",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub kind: EventKind,
    pub data: Arc<[u8]>,
}

#[derive(Debug)]
pub enum EventError {
    TooManyClients,
    TooLarge,
    Serialization(serde_json::Error),
}

impl Event {
    pub fn serialize<T: serde::Serialize>(kind: EventKind, value: &T) -> Result<Self, EventError> {
        let data = serde_json::to_vec(value).map_err(EventError::Serialization)?;
        if data.len() > MAX_EVENT_BYTES {
            return Err(EventError::TooLarge);
        }
        Ok(Self {
            kind,
            data: data.into(),
        })
    }
}

struct Client {
    events: mpsc::Sender<Event>,
    meter: watch::Sender<Option<Event>>,
    usb: watch::Sender<Option<Event>>,
    disconnected: watch::Sender<bool>,
}

/// Owned by the control actor. Its bounded queues share immutable event bytes.
#[derive(Default)]
pub struct Hub {
    clients: Vec<Client>,
}

pub struct Subscription {
    events: mpsc::Receiver<Event>,
    meter: watch::Receiver<Option<Event>>,
    usb: watch::Receiver<Option<Event>>,
    disconnected: watch::Receiver<bool>,
}

impl Hub {
    /// Control-thread hint; subscribing and publishing share the facade lock.
    /// Closed streams must not keep otherwise-unobserved serialization alive.
    pub fn has_clients(&mut self) -> bool {
        self.clients.retain(|client| !client.events.is_closed());
        !self.clients.is_empty()
    }
    /// Call while the controller owns the current-state snapshot so subscribing
    /// and enqueueing that snapshot cannot miss an intervening state change.
    pub fn subscribe(
        &mut self,
        state: &rc2fx_core::DeviceState,
    ) -> Result<Subscription, EventError> {
        self.has_clients();
        if self.clients.len() >= MAX_CLIENTS {
            return Err(EventError::TooManyClients);
        }
        let initial = Event::serialize(EventKind::State, state)?;
        let (events, receiver) = mpsc::channel(QUEUED_EVENTS);
        let (meter, meter_receiver) = watch::channel(None);
        // Do not replay old hardware observations as fresh on reconnect.
        let (usb, usb_receiver) = watch::channel(None);
        let (disconnected, disconnect_receiver) = watch::channel(false);
        // A newly created queue has room for its initial state.
        if events.try_send(initial).is_err() {
            unreachable!("new bounded queue rejected its first event");
        }
        self.clients.push(Client {
            events,
            meter,
            usb,
            disconnected,
        });
        Ok(Subscription {
            events: receiver,
            meter: meter_receiver,
            usb: usb_receiver,
            disconnected: disconnect_receiver,
        })
    }

    pub fn publish(&mut self, event: Event) {
        self.clients.retain(|client| {
            if client.events.is_closed() {
                return false;
            }
            if event.kind == EventKind::Meter {
                // Keep only the latest meter; slow clients cannot accumulate audio telemetry.
                return client.meter.send(Some(event.clone())).is_ok();
            }
            if event.kind == EventKind::Usb {
                return client.usb.send(Some(event.clone())).is_ok();
            }
            if client.events.try_send(event.clone()).is_err() {
                // Missing a reliable state update invalidates the entire stream.
                client.disconnected.send_replace(true);
                return false;
            }
            true
        });
    }

    pub fn disconnect_all(&mut self) {
        for client in self.clients.drain(..) {
            client.disconnected.send_replace(true);
        }
    }
}

impl Subscription {
    pub async fn next(&mut self) -> Option<Event> {
        if *self.disconnected.borrow() {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.disconnected.changed() => None,
            event = self.events.recv() => event,
            result = self.usb.changed() => {
                result.ok()?;
                self.usb.borrow_and_update().clone()
            }
            result = self.meter.changed() => {
                result.ok()?;
                self.meter.borrow_and_update().clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::*;

    fn state() -> DeviceState {
        DeviceState {
            config: Config::default(),
            engine: EngineStatus {
                generation: 0,
                restart_allowed: false,
                mode: EngineMode::Offline,
                status: EnginePhase::Offline,
                applied_revision: 0,
                sample_rate: 0,
                block_size: 0,
                message: "No engine started".into(),
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
                qualified_target_ids: vec![],
                active_target_ids: vec![],
                pending_target_ids: vec![],
                message: String::new(),
            },
        }
    }

    fn event(kind: EventKind, n: u64) -> Event {
        Event::serialize(kind, &serde_json::json!({"n": n})).unwrap()
    }

    #[tokio::test]
    async fn usb_observations_coalesce_independently_and_do_not_replay_on_reconnect() {
        let mut hub = Hub::default();
        let mut sub = hub.subscribe(&state()).unwrap();
        assert_eq!(sub.next().await.unwrap().kind, EventKind::State);
        for n in 0..1000 {
            hub.publish(event(EventKind::Usb, n));
            hub.publish(event(EventKind::Meter, n));
        }
        let usb = sub.next().await.unwrap();
        assert_eq!(usb.kind, EventKind::Usb);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&usb.data).unwrap()["n"],
            999
        );
        assert_eq!(sub.next().await.unwrap().kind, EventKind::Meter);
        drop(sub);
        let mut new = hub.subscribe(&state()).unwrap();
        assert_eq!(new.next().await.unwrap().kind, EventKind::State);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), new.next())
                .await
                .is_err()
        );
        hub.publish(event(EventKind::Usb, 1001));
        assert_eq!(new.next().await.unwrap().kind, EventKind::Usb);
    }

    #[tokio::test]
    async fn subscription_gets_state_then_only_latest_meter() {
        let mut hub = Hub::default();
        let snapshot = state();
        let mut subscription = hub.subscribe(&snapshot).unwrap();
        for n in 0..1000 {
            hub.publish(event(EventKind::Meter, n));
        }
        let first = subscription.next().await.unwrap();
        assert_eq!(first.kind, EventKind::State);
        assert_eq!(
            serde_json::from_slice::<DeviceState>(&first.data).unwrap(),
            snapshot
        );
        let meter = subscription.next().await.unwrap();
        assert_eq!(meter.kind, EventKind::Meter);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&meter.data).unwrap()["n"],
            999
        );
    }

    #[tokio::test]
    async fn reliable_overflow_closes_slow_client_without_affecting_fast_client() {
        let mut hub = Hub::default();
        let mut slow = hub.subscribe(&state()).unwrap();
        let mut fast = hub.subscribe(&state()).unwrap();
        fast.next().await.unwrap();
        for n in 0..QUEUED_EVENTS as u64 {
            hub.publish(event(EventKind::Engine, n));
            assert_eq!(fast.next().await.unwrap().kind, EventKind::Engine);
        }
        assert!(
            slow.next().await.is_none(),
            "overflow must discard even queued stale state"
        );
        hub.publish(event(EventKind::Meter, 42));
        assert_eq!(fast.next().await.unwrap().kind, EventKind::Meter);
    }

    #[tokio::test]
    async fn client_limit_reclaimed_on_drop_and_explicit_disconnect() {
        let mut hub = Hub::default();
        let mut clients: Vec<_> = (0..MAX_CLIENTS)
            .map(|_| hub.subscribe(&state()).unwrap())
            .collect();
        assert!(matches!(
            hub.subscribe(&state()),
            Err(EventError::TooManyClients)
        ));
        clients.pop();
        clients.push(hub.subscribe(&state()).unwrap());
        hub.disconnect_all();
        for client in &mut clients {
            assert!(client.next().await.is_none());
        }
        assert!(hub.subscribe(&state()).is_ok());
    }

    #[test]
    fn event_size_limit_is_enforced() {
        assert!(matches!(
            Event::serialize(EventKind::State, &"x".repeat(MAX_EVENT_BYTES)),
            Err(EventError::TooLarge)
        ));
    }
}
