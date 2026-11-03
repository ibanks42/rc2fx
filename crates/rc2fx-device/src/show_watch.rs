//! Confirmed stock-show removals disable processing through the control owner.
//! Reads never modify the vendor show. Two matching consecutive complete reads
//! establish initial assignment authority; unavailable reads cannot disable audio.
use crate::{
    coordinator::{Handle, receive},
    stock_inputs,
};
use rc2fx_core::{DeviceState, SelectionRequest};
use std::{collections::HashSet, path::PathBuf, time::Duration};

/// Identity-only state; bounded by the configured processing target catalog.
#[derive(Default)]
pub struct Watch {
    assigned: Option<HashSet<String>>,
    established: bool,
    pending: HashSet<String>,
}
impl Watch {
    /// Supply only complete, validated observations. `None` invalidates the
    /// initial confirmation candidate and never submits a mutation, even if a
    /// removal is pending after authority has already been established.
    pub fn observe(
        &mut self,
        state: &DeviceState,
        assigned: Option<HashSet<String>>,
    ) -> Option<SelectionRequest> {
        let Some(assigned) = assigned else {
            if !self.established {
                self.assigned = None;
            }
            return None;
        };
        let known: HashSet<_> = state
            .config
            .targets
            .iter()
            .filter(|t| {
                t.kind != rc2fx_core::TargetKind::Output
                    && state.selection.qualified_target_ids.contains(&t.id)
            })
            .map(|t| t.id.clone())
            .collect();
        let assigned: HashSet<_> = assigned.intersection(&known).cloned().collect();
        if !self.established {
            if self.assigned.as_ref() != Some(&assigned) {
                self.assigned = Some(assigned);
                return None;
            }
            self.established = true;
        }
        // Also reconcile initially stale selections, or a later explicit
        // enable of a source that the confirmed show no longer assigns.
        self.pending.extend(known.difference(&assigned).cloned());
        self.assigned = Some(assigned.clone());
        // Reappearance cancels a queued removal, but never turns a chain on.
        // An already-disabled target needs no retry, and disappears from debt.
        self.pending.retain(|id| {
            known.contains(id)
                && !assigned.contains(id)
                && state.config.selected_targets.contains(id)
        });
        if self.pending.is_empty() || !state.selection.allowed {
            return None;
        }
        let target_ids: Vec<_> = state
            .config
            .selected_targets
            .iter()
            .filter(|id| !self.pending.contains(*id))
            .cloned()
            .collect();
        Some(SelectionRequest {
            expected_revision: state.config.revision,
            expected_generation: state.engine.generation,
            target_ids,
        })
    }
}

/// One poll/read/transaction at a time. Awaiting the blocking reader before
/// starting the next tick prevents task buildup even if a filesystem read is
/// slow. Busy/conflict/failure leaves identity debt for the next fresh read.
pub async fn run(
    handle: Handle,
    path: PathBuf,
    catalog: Vec<stock_inputs::CatalogChannel<'static>>,
) {
    let mut watcher = Watch::default();
    let (mut read_failed, mut selection_failed) = (false, false);
    loop {
        let Ok(reply) = handle.state() else { return };
        let Ok(state) = receive(Ok(reply)).await else {
            return;
        };
        let read_path = path.clone();
        let config = state.config.clone();
        let read_catalog = catalog.clone();
        let read = tokio::task::spawn_blocking(move || {
            let inventory = stock_inputs::read(&read_path, &config, &read_catalog);
            if !inventory.available || inventory.validate().is_err() {
                return None;
            }
            stock_inputs::assigned_targets(&inventory, &config, &read_catalog)
                .map(|ids| ids.into_iter().collect())
        })
        .await;
        // Refresh after disk work: unrelated desktop saves must not be
        // overwritten with the revision/selection from before the read.
        let Ok(reply) = handle.state() else { return };
        let Ok(state) = receive(Ok(reply)).await else {
            return;
        };
        let observation = read.ok().flatten();
        if observation.is_none() && !read_failed {
            eprintln!("Stock show observer: read unavailable; processing unchanged, retrying");
        } else if observation.is_some() && read_failed {
            eprintln!("Stock show observer: complete reads resumed");
        }
        read_failed = observation.is_none();
        if let Some(request) = watcher.observe(&state, observation) {
            // The selection transaction owns persistence and DSP publication.
            // Do not acknowledge debt optimistically: the next state decides.
            let failed = receive(handle.selection(request)).await.is_err();
            if failed && !selection_failed {
                eprintln!(
                    "Stock show observer: removal selection deferred; retrying after a fresh read"
                );
            } else if !failed && selection_failed {
                eprintln!("Stock show observer: removal selection accepted");
            }
            selection_failed = failed;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{controller::Controller, store::Store};
    use rc2fx_core::Config;

    fn assigned(ids: &[&str]) -> Option<HashSet<String>> {
        Some(ids.iter().map(|s| (*s).into()).collect())
    }
    fn state() -> DeviceState {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            selected_targets: vec!["mic-1".into(), "usb-main".into(), "send-chat".into()],
            ..Config::default()
        };
        let mut state = Controller::new(Store::open(dir.path(), config).unwrap()).state();
        state.selection.allowed = true;
        state.selection.qualified_target_ids =
            state.config.targets.iter().map(|t| t.id.clone()).collect();
        state.engine.generation = 7;
        state
    }

    #[test]
    fn baseline_move_and_failed_reads_do_not_disable() {
        let state = state();
        let mut w = Watch::default();
        assert!(w.observe(&state, None).is_none());
        assert!(
            w.observe(&state, assigned(&["mic-1", "usb-main"]))
                .is_none()
        );
        assert!(
            w.observe(&state, assigned(&["usb-main", "mic-1"]))
                .is_none()
        );
        assert!(w.observe(&state, None).is_none());
        assert!(
            w.observe(&state, assigned(&["mic-1", "usb-main"]))
                .is_none()
        );
        // First valid observation does not disable an already absent input.
        assert!(Watch::default().observe(&state, assigned(&[])).is_none());
    }

    #[test]
    fn unqualified_config_targets_are_never_disabled() {
        let mut state = state();
        state
            .selection
            .qualified_target_ids
            .retain(|id| id != "mic-1");
        let mut w = Watch::default();
        assert!(w.observe(&state, assigned(&[])).is_none());
        let request = w.observe(&state, assigned(&[])).unwrap();
        assert_eq!(request.target_ids, ["mic-1", "send-chat"]);
        state.selection.qualified_target_ids.clear();
        assert!(w.observe(&state, assigned(&[])).is_none());
    }

    #[test]
    fn initial_stale_selection_needs_two_consecutive_matching_valid_reads() {
        let state = state();
        let mut w = Watch::default();
        assert!(w.observe(&state, assigned(&[])).is_none());
        assert!(w.observe(&state, None).is_none());
        assert!(w.observe(&state, assigned(&[])).is_none());
        // A different complete snapshot restarts the confirmation pair.
        assert!(w.observe(&state, assigned(&["usb-main"])).is_none());
        let request = w.observe(&state, assigned(&["usb-main"])).unwrap();
        assert_eq!(request.target_ids, ["usb-main", "send-chat"]);
        // Outputs cannot be introduced to removal debt by bad caller input.
        assert!(w.pending.iter().all(|id| id != "send-chat"));
    }

    #[test]
    fn removing_every_input_never_removes_selected_outputs() {
        let state = state();
        let mut w = Watch::default();
        assert!(w.observe(&state, assigned(&[])).is_none());
        let request = w.observe(&state, assigned(&[])).unwrap();
        assert_eq!(request.target_ids, ["send-chat"]);
    }

    #[test]
    fn removal_only_changes_selection_and_keeps_outputs() {
        let mut state = state();
        state.config.targets[0].gain.db = -7.5;
        let effects = state.config.targets.clone();
        let mut w = Watch::default();
        w.observe(&state, assigned(&["mic-1", "usb-main"]));
        w.observe(&state, assigned(&["mic-1", "usb-main"]));
        let request = w.observe(&state, assigned(&["usb-main"])).unwrap();
        assert_eq!(request.target_ids, ["usb-main", "send-chat"]);
        assert_eq!(request.expected_revision, state.config.revision);
        assert_eq!(request.expected_generation, 7);
        assert_eq!(state.config.targets, effects);
        state.config.selected_targets = request.target_ids;
        assert!(w.observe(&state, assigned(&["usb-main"])).is_none());
        assert!(
            w.observe(&state, assigned(&["usb-main", "mic-1"]))
                .is_none()
        );
        assert_eq!(state.config.targets, effects);
        assert!(!state.config.selected_targets.contains(&"mic-1".into()));
    }

    #[test]
    fn pending_removal_survives_busy_conflict_and_failed_read_but_cancels_on_return() {
        let mut state = state();
        let mut w = Watch::default();
        w.observe(&state, assigned(&["mic-1", "usb-main"]));
        w.observe(&state, assigned(&["mic-1", "usb-main"]));
        state.selection.allowed = false;
        assert!(w.observe(&state, assigned(&["usb-main"])).is_none());
        state.selection.allowed = true;
        let rejected = w.observe(&state, assigned(&["usb-main"])).unwrap();
        // Busy/conflict means the state did not accept this request.
        state.config.revision += 1;
        state.config.targets[0].gain.db = -4.;
        assert!(w.observe(&state, None).is_none());
        let retried = w.observe(&state, assigned(&["usb-main"])).unwrap();
        assert_eq!(retried.expected_revision, rejected.expected_revision + 1);
        assert_eq!(retried.target_ids, rejected.target_ids);
        assert_eq!(state.config.targets[0].gain.db, -4.);
        assert!(
            w.observe(&state, assigned(&["usb-main", "mic-1"]))
                .is_none()
        );
        assert!(w.pending.is_empty());
    }
}
