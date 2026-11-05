//! Confirmed stock-show removals disable processing through the control owner.
//! Reads never modify the vendor show. Two matching consecutive complete reads
//! establish initial assignment authority; unavailable reads cannot disable audio.
use crate::{
    coordinator::{Handle, receive},
    stock_inputs,
};
use rc2fx_core::{DeviceState, SelectionRequest};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// A write inside one timestamp tick can leave the identity unchanged, so a
/// file this recently written is always read again.
const SETTLE_TIME: Duration = Duration::from_secs(2);

/// What must stay the same for the file's contents to be the same.
#[derive(Clone, PartialEq, Eq)]
struct FileIdentity {
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
}
fn file_identity(path: &Path) -> Option<FileIdentity> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        size: metadata.len(),
        modified: metadata.modified().ok()?,
        #[cfg(unix)]
        unix: (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
    })
}

struct Reading {
    identity: FileIdentity,
    revision: u64,
    assignment: HashSet<String>,
}
/// Skips reading and parsing a show that cannot have changed. A reading is
/// reused only after two consecutive complete reads of the same file agreed,
/// under the same settings, so skipping can never supply the second read of
/// the authority rule. A failed or differing read forgets it.
#[derive(Default)]
struct UnchangedShow {
    last: Option<Reading>,
    confirmed: bool,
}
impl UnchangedShow {
    fn reusable(&self, identity: Option<&FileIdentity>, revision: u64) -> Option<HashSet<String>> {
        let last = self.last.as_ref()?;
        (self.confirmed && identity == Some(&last.identity) && last.revision == revision)
            .then(|| last.assignment.clone())
    }
    fn record(
        &mut self,
        before: Option<FileIdentity>,
        after: Option<FileIdentity>,
        revision: u64,
        assignment: Option<&HashSet<String>>,
        now: SystemTime,
    ) {
        let stable = before.filter(|identity| {
            Some(identity) == after.as_ref()
                && now
                    .duration_since(identity.modified)
                    .is_ok_and(|age| age >= SETTLE_TIME)
        });
        let (Some(identity), Some(assignment)) = (stable, assignment) else {
            *self = Self::default();
            return;
        };
        self.confirmed = self.last.as_ref().is_some_and(|last| {
            last.identity == identity && last.revision == revision && &last.assignment == assignment
        });
        self.last = Some(Reading {
            identity,
            revision,
            assignment: assignment.clone(),
        });
    }
}

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
    let mut unchanged = Some(UnchangedShow::default());
    let (mut read_failed, mut selection_failed) = (false, false);
    loop {
        let Ok(reply) = handle.state() else { return };
        let Ok(state) = receive(Ok(reply)).await else {
            return;
        };
        let read_path = path.clone();
        let config = state.config.clone();
        let read_catalog = catalog.clone();
        let mut show = unchanged.take().unwrap_or_default();
        let read = tokio::task::spawn_blocking(move || {
            let before = file_identity(&read_path);
            if let Some(assignment) = show.reusable(before.as_ref(), config.revision) {
                return (Some(assignment), show);
            }
            let inventory = stock_inputs::read(&read_path, &config, &read_catalog);
            let assignment: Option<HashSet<String>> =
                if !inventory.available || inventory.validate().is_err() {
                    None
                } else {
                    stock_inputs::assigned_targets(&inventory, &config, &read_catalog)
                        .map(|ids| ids.into_iter().collect())
                };
            let after = file_identity(&read_path);
            show.record(
                before,
                after,
                config.revision,
                assignment.as_ref(),
                SystemTime::now(),
            );
            (assignment, show)
        })
        .await;
        // Refresh after disk work: unrelated desktop saves must not be
        // overwritten with the revision/selection from before the read.
        let Ok(reply) = handle.state() else { return };
        let Ok(state) = receive(Ok(reply)).await else {
            return;
        };
        // A lost task loses its memory too: the next tick simply reads.
        let observation = read.ok().and_then(|(assignment, show)| {
            unchanged = Some(show);
            assignment
        });
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

    fn identity(size: u64) -> FileIdentity {
        FileIdentity {
            size,
            modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            unix: (1, 2, 3, 4),
        }
    }
    fn recorded(show: &mut UnchangedShow, size: u64, revision: u64, ids: &[&str]) {
        show.record(
            Some(identity(size)),
            Some(identity(size)),
            revision,
            assigned(ids).as_ref(),
            SystemTime::now(),
        );
    }

    #[test]
    fn a_show_is_reused_only_after_two_matching_complete_reads() {
        let mut show = UnchangedShow::default();
        assert!(show.reusable(Some(&identity(1)), 7).is_none());
        recorded(&mut show, 1, 7, &["mic-1"]);
        assert!(show.reusable(Some(&identity(1)), 7).is_none());
        recorded(&mut show, 1, 7, &["mic-1"]);
        assert_eq!(show.reusable(Some(&identity(1)), 7), assigned(&["mic-1"]));
    }

    #[test]
    fn any_change_or_failure_returns_to_reading_the_file() {
        let mut show = UnchangedShow::default();
        recorded(&mut show, 1, 7, &["mic-1"]);
        recorded(&mut show, 1, 7, &["mic-1"]);
        assert!(show.reusable(Some(&identity(2)), 7).is_none());
        assert!(show.reusable(Some(&identity(1)), 8).is_none());
        assert!(show.reusable(None, 7).is_none());
        // The same file read differently is not confirmed.
        recorded(&mut show, 1, 7, &["usb-main"]);
        assert!(show.reusable(Some(&identity(1)), 7).is_none());
        recorded(&mut show, 1, 7, &["usb-main"]);
        show.record(
            Some(identity(1)),
            Some(identity(1)),
            7,
            None,
            SystemTime::now(),
        );
        assert!(show.reusable(Some(&identity(1)), 7).is_none());
    }

    #[test]
    fn a_file_changing_during_the_read_or_just_written_is_not_remembered() {
        let mut show = UnchangedShow::default();
        recorded(&mut show, 1, 7, &["mic-1"]);
        show.record(
            Some(identity(1)),
            Some(identity(2)),
            7,
            assigned(&["mic-1"]).as_ref(),
            SystemTime::now(),
        );
        assert!(show.last.is_none());
        let mut fresh = identity(1);
        fresh.modified = SystemTime::now();
        for _ in 0..2 {
            show.record(
                Some(fresh.clone()),
                Some(fresh.clone()),
                7,
                assigned(&["mic-1"]).as_ref(),
                SystemTime::now(),
            );
        }
        assert!(show.reusable(Some(&fresh), 7).is_none());
    }

    #[test]
    fn a_rewritten_file_has_a_different_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("show");
        std::fs::write(&path, "a").unwrap();
        let first = file_identity(&path).unwrap();
        assert!(first == file_identity(&path).unwrap());
        std::fs::write(dir.path().join("next"), "bb").unwrap();
        std::fs::rename(dir.path().join("next"), &path).unwrap();
        assert!(first != file_identity(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(file_identity(&path).is_none());
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
