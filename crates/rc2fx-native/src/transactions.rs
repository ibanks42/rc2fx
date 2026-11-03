//! Bounded control-side staging. No Store, audio work, timers or callback locks.
use crate::parameters;
use rc2fx_core::Target;
use rc2fx_native_control::{ErrorCode, PathMode, TargetStatus, Transaction};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const LIMIT: usize = 24;
const LEASE: Duration = Duration::from_secs(30);
pub trait Endpoints {
    fn prepare(
        &mut self,
        transaction: &Transaction,
        target: &Target,
        mode: PathMode,
    ) -> Result<parameters::Staged, ErrorCode>;
    /// Leave the slot intact on lifecycle contention before taking ownership.
    fn commit(
        &mut self,
        transaction: &Transaction,
        staged: &mut Option<parameters::Staged>,
    ) -> Result<(), ErrorCode>;
    fn status(&mut self, transaction: &Transaction) -> Result<TargetStatus, ErrorCode>;
}
struct Pending {
    transaction: Transaction,
    fingerprint: [u8; 32],
    staged: Option<parameters::Staged>,
    expires: Instant,
}
struct Completed {
    transaction: Transaction,
    fingerprint: [u8; 32],
}
#[derive(Default)]
pub struct Manager {
    pending: Vec<Pending>,
    completed: Vec<Completed>,
}
impl Manager {
    /// Request-driven expiry: no periodic wakeups when a controller disappears.
    pub fn reap(&mut self, now: Instant) {
        self.pending.retain(|pending| pending.expires > now);
    }
    pub fn prepare(
        &mut self,
        transaction: &Transaction,
        target: &Target,
        mode: PathMode,
        now: Instant,
        endpoints: &mut impl Endpoints,
    ) -> Result<(), ErrorCode> {
        self.reap(now);
        if !transaction.valid() || transaction.target_id != target.id {
            return Err(ErrorCode::InvalidParameters);
        }
        let fingerprint: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&(target, mode)).map_err(|_| ErrorCode::InvalidParameters)?,
        )
        .into();
        if let Some(pending) = self
            .pending
            .iter()
            .find(|pending| pending.transaction.id == transaction.id)
        {
            return if pending.transaction == *transaction && pending.fingerprint == fingerprint {
                // A retransmission never extends a lease or allocates another snapshot.
                endpoints.status(transaction).map(|_| ())
            } else {
                Err(ErrorCode::InvalidParameters)
            };
        }
        if let Some(completed) = self
            .completed
            .iter()
            .find(|completed| completed.transaction.id == transaction.id)
        {
            if completed.transaction != *transaction || completed.fingerprint != fingerprint {
                return Err(ErrorCode::InvalidParameters);
            }
            return Self::confirmed(transaction, endpoints);
        }
        if self.pending.len() == LIMIT {
            return Err(ErrorCode::Busy);
        }
        let staged = endpoints.prepare(transaction, target, mode)?;
        self.pending.push(Pending {
            transaction: transaction.clone(),
            fingerprint,
            staged: Some(staged),
            expires: now + LEASE,
        });
        Ok(())
    }
    fn confirmed(
        transaction: &Transaction,
        endpoints: &mut impl Endpoints,
    ) -> Result<(), ErrorCode> {
        let status = endpoints.status(transaction)?;
        if status.published != transaction.revision {
            return Err(ErrorCode::StaleRevision);
        }
        Ok(())
    }
    pub fn commit(
        &mut self,
        transaction: &Transaction,
        now: Instant,
        endpoints: &mut impl Endpoints,
    ) -> Result<(), ErrorCode> {
        self.reap(now);
        if let Some(completed) = self
            .completed
            .iter()
            .find(|completed| completed.transaction.id == transaction.id)
        {
            if completed.transaction != *transaction {
                return Err(ErrorCode::InvalidParameters);
            }
            return Self::confirmed(transaction, endpoints);
        }
        let index = self
            .pending
            .iter()
            .position(|pending| pending.transaction.id == transaction.id)
            .ok_or(ErrorCode::Unavailable)?;
        if self.pending[index].transaction != *transaction {
            return Err(ErrorCode::InvalidParameters);
        }
        if let Err(error) = endpoints.commit(transaction, &mut self.pending[index].staged) {
            if self.pending[index].staged.is_none() {
                self.pending.swap_remove(index);
            }
            return Err(error);
        }
        let pending = self.pending.swap_remove(index);
        // Keep only the latest receipt for a path, plus a bounded recent cache.
        self.completed.retain(|completed| {
            (
                completed.transaction.context,
                &completed.transaction.target_id,
            ) != (transaction.context, &transaction.target_id)
        });
        if self.completed.len() == LIMIT {
            self.completed.remove(0);
        }
        self.completed.push(Completed {
            transaction: pending.transaction,
            fingerprint: pending.fingerprint,
        });
        Ok(())
    }
    pub fn cancel(&mut self, transaction: &Transaction, now: Instant) -> Result<(), ErrorCode> {
        self.reap(now);
        if let Some(completed) = self
            .completed
            .iter()
            .find(|completed| completed.transaction.id == transaction.id)
        {
            return Err(if completed.transaction == *transaction {
                ErrorCode::StaleRevision
            } else {
                ErrorCode::InvalidParameters
            });
        }
        if let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.transaction.id == transaction.id)
        {
            if self.pending[index].transaction != *transaction {
                return Err(ErrorCode::InvalidParameters);
            }
            self.pending.swap_remove(index);
        }
        // Cancel is idempotent and cannot undo an already-published change.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::TargetKind;
    use rc2fx_dsp::Prepared;
    use rc2fx_native_control::Session;
    struct Paths {
        control: parameters::Control,
        reader: parameters::Reader,
        busy: bool,
        live: bool,
        context: u64,
    }
    impl Paths {
        fn new(target: &Target) -> Self {
            let (control, reader) =
                parameters::channel(target, Prepared::new(target, 48_000).unwrap());
            Self {
                control,
                reader,
                busy: false,
                live: true,
                context: 7,
            }
        }
        fn frame(&mut self) {
            assert!(self.reader.begin());
            self.reader.finish(true);
        }
    }
    impl Endpoints for Paths {
        fn prepare(
            &mut self,
            transaction: &Transaction,
            target: &Target,
            mode: PathMode,
        ) -> Result<parameters::Staged, ErrorCode> {
            self.status(transaction)?;
            self.control
                .prepare_mode(target, transaction.revision, mode)
                .map_err(crate::parameter_error)
        }
        fn commit(
            &mut self,
            transaction: &Transaction,
            staged: &mut Option<parameters::Staged>,
        ) -> Result<(), ErrorCode> {
            self.status(transaction)?;
            if self.busy {
                self.busy = false;
                return Err(ErrorCode::Busy);
            }
            self.control
                .reserve(staged.take().unwrap())
                .map_err(crate::parameter_error)?
                .commit();
            Ok(())
        }
        fn status(&mut self, transaction: &Transaction) -> Result<TargetStatus, ErrorCode> {
            if !self.live
                || transaction.context != self.context
                || transaction.target_id != self.control.id()
            {
                return Err(ErrorCode::Unavailable);
            }
            crate::target_status(&mut self.control)
        }
    }
    fn target() -> Target {
        Target::new("source", "Source", TargetKind::Mic, 1)
    }
    fn transaction() -> Transaction {
        Transaction {
            session: Session {
                pid: 42,
                nonce: "a".repeat(32),
            },
            id: "b".repeat(32),
            context: 7,
            target_id: "source".into(),
            revision: 2,
        }
    }
    #[test]
    fn preparation_and_cancellation_do_not_publish_and_release_the_snapshot_slot() {
        let target = target();
        let transaction = transaction();
        let now = Instant::now();
        let mut paths = Paths::new(&target);
        paths.frame();
        let mut manager = Manager::default();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        assert_eq!(manager.pending.len(), 1);
        assert_eq!(paths.control.status().unwrap().published, 1);
        paths.frame();
        assert_eq!(paths.control.status().unwrap().applied, 1);
        manager.cancel(&transaction, now).unwrap();
        manager.cancel(&transaction, now).unwrap();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        manager.commit(&transaction, now, &mut paths).unwrap();
        assert_eq!(paths.control.status().unwrap().published, 2);
        assert_eq!(paths.control.status().unwrap().applied, 1);
        paths.frame();
        assert_eq!(paths.control.status().unwrap().applied, 2);
    }
    #[test]
    fn lost_commit_reply_is_reconcilable_and_cannot_be_cancelled_after_publication() {
        let target = target();
        let transaction = transaction();
        let now = Instant::now();
        let mut paths = Paths::new(&target);
        let mut manager = Manager::default();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        manager.commit(&transaction, now, &mut paths).unwrap();
        manager.commit(&transaction, now, &mut paths).unwrap();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        assert_eq!(manager.pending.len(), 0);
        assert_eq!(manager.completed.len(), 1);
        assert_eq!(
            manager.cancel(&transaction, now),
            Err(ErrorCode::StaleRevision)
        );
        assert_eq!(paths.control.status().unwrap().published, 2);
        paths.live = false;
        assert_eq!(
            manager.commit(&transaction, now, &mut paths),
            Err(ErrorCode::Unavailable)
        );
    }
    #[test]
    fn expiry_releases_abandoned_candidates_without_periodic_work() {
        let target = target();
        let transaction = transaction();
        let now = Instant::now();
        let mut paths = Paths::new(&target);
        let mut manager = Manager::default();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        let later = now + LEASE;
        assert_eq!(
            manager.commit(&transaction, later, &mut paths),
            Err(ErrorCode::Unavailable)
        );
        assert_eq!(paths.control.status().unwrap().published, 1);
        manager
            .prepare(
                &transaction,
                &target,
                PathMode::Processed,
                later,
                &mut paths,
            )
            .unwrap();
    }
    #[test]
    fn receipt_or_payload_reuse_is_refused_and_lifecycle_contention_keeps_the_candidate() {
        let target = target();
        let transaction = transaction();
        let now = Instant::now();
        let mut paths = Paths::new(&target);
        let mut manager = Manager::default();
        manager
            .prepare(&transaction, &target, PathMode::Processed, now, &mut paths)
            .unwrap();
        let mut changed = target.clone();
        changed.eq.input_gain_db = -6.0;
        assert_eq!(
            manager.prepare(&transaction, &changed, PathMode::Processed, now, &mut paths),
            Err(ErrorCode::InvalidParameters)
        );
        assert_eq!(
            manager.prepare(
                &transaction,
                &target,
                PathMode::Passthrough,
                now,
                &mut paths
            ),
            Err(ErrorCode::InvalidParameters),
            "receipt replay must include the requested mode"
        );
        let mut wrong = transaction.clone();
        wrong.revision += 1;
        assert_eq!(
            manager.commit(&wrong, now, &mut paths),
            Err(ErrorCode::InvalidParameters)
        );
        assert_eq!(
            manager.cancel(&wrong, now),
            Err(ErrorCode::InvalidParameters)
        );
        paths.busy = true;
        assert_eq!(
            manager.commit(&transaction, now, &mut paths),
            Err(ErrorCode::Busy)
        );
        assert!(manager.pending[0].staged.is_some());
        manager.commit(&transaction, now, &mut paths).unwrap();
    }
    #[test]
    fn pending_snapshots_and_recent_commit_receipts_have_independent_fixed_bounds() {
        let target = target();
        let now = Instant::now();
        let mut manager = Manager::default();
        for index in 1..=LIMIT + 1 {
            let mut paths = Paths::new(&target);
            paths.context = index as u64;
            let mut transaction = transaction();
            transaction.context = index as u64;
            transaction.id = format!("{index:032x}");
            let result =
                manager.prepare(&transaction, &target, PathMode::Processed, now, &mut paths);
            if index <= LIMIT {
                result.unwrap();
            } else {
                assert_eq!(result, Err(ErrorCode::Busy));
            }
        }
        assert_eq!(manager.pending.len(), LIMIT);
        manager.reap(now + LEASE);
        assert!(manager.pending.is_empty());
        for index in 1..=LIMIT + 1 {
            let mut paths = Paths::new(&target);
            paths.context = index as u64;
            let mut transaction = transaction();
            transaction.context = index as u64;
            transaction.id = format!("{index:032x}");
            manager
                .prepare(
                    &transaction,
                    &target,
                    PathMode::Processed,
                    now + LEASE,
                    &mut paths,
                )
                .unwrap();
            manager
                .commit(&transaction, now + LEASE, &mut paths)
                .unwrap();
        }
        assert_eq!(manager.completed.len(), LIMIT);
        assert!(manager.pending.is_empty());
    }
}
