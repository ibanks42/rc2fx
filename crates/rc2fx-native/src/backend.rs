//! IPC backend adapter connecting `ipc::Server` to live native audio contexts.
use super::*;

pub(crate) fn parameter_error(error: parameters::Error) -> rc2fx_native_control::ErrorCode {
    match error {
        parameters::Error::WrongTarget => rc2fx_native_control::ErrorCode::Unavailable,
        parameters::Error::Busy => rc2fx_native_control::ErrorCode::Busy,
        parameters::Error::SuppressionInUse => rc2fx_native_control::ErrorCode::SuppressionInUse,
        parameters::Error::RequiresReset => rc2fx_native_control::ErrorCode::RequiresReset,
        parameters::Error::StaleRevision => rc2fx_native_control::ErrorCode::StaleRevision,
        parameters::Error::Invalid => rc2fx_native_control::ErrorCode::InvalidParameters,
        parameters::Error::Poisoned | parameters::Error::Runtime => {
            rc2fx_native_control::ErrorCode::Internal
        }
    }
}
fn lock_error<T>(error: std::sync::TryLockError<T>) -> rc2fx_native_control::ErrorCode {
    match error {
        std::sync::TryLockError::WouldBlock => rc2fx_native_control::ErrorCode::Busy,
        std::sync::TryLockError::Poisoned(_) => rc2fx_native_control::ErrorCode::Internal,
    }
}
pub(crate) fn target_status(
    control: &mut parameters::Control,
) -> Result<rc2fx_native_control::TargetStatus, rc2fx_native_control::ErrorCode> {
    let status = control.status().map_err(parameter_error)?;
    let worker = control.worker_status();
    let worker_recent_max_ns = control.worker_recent_turnaround_ns();
    // Histories from a worker other than the status's runtime (one replaced
    // between the two reads) are left out: a reader keys them on the epoch.
    let (misses, lates) = match control.worker_histories() {
        Some((epoch, misses, lates)) if epoch == status.runtime_epoch => {
            (Some(misses), Some(lates))
        }
        _ => (None, None),
    };
    Ok(rc2fx_native_control::TargetStatus {
        id: control.id().into(),
        published: status.published,
        seen: status.seen,
        applied: status.applied,
        fingerprint: status.fingerprint,
        mode: status.mode,
        runtime_epoch: status.runtime_epoch,
        meter: status.meter,
        worker,
        worker_recent_max_ns,
        misses,
        lates,
        retired_workers: control.retired_workers(),
        retired_worker_count: control.retired_worker_count(),
    })
}
fn with_parameter_control<T>(
    token: u64,
    id: &str,
    apply: impl FnOnce(&mut parameters::Control) -> Result<T, rc2fx_native_control::ErrorCode>,
) -> Result<T, rc2fx_native_control::ErrorCode> {
    // Return backpressure instead of waiting behind a native lifecycle call.
    let _registry = CONTROL.try_lock().map_err(lock_error)?;
    let Some(context) = CONTEXTS.iter().find(|context| {
        context.slot.load(Ordering::Acquire).is_live()
            && context.token.load(Ordering::Relaxed) == token
    }) else {
        return Err(rc2fx_native_control::ErrorCode::Unavailable);
    };
    let mut controls = context.parameters.try_lock().map_err(lock_error)?;
    let Some(control) = controls.iter_mut().find(|control| control.id() == id) else {
        return Err(rc2fx_native_control::ErrorCode::Unavailable);
    };
    apply(control)
}
#[derive(Default)]
struct NativeBackend {
    transactions: transactions::Manager,
}
struct LiveEndpoints;
impl transactions::Endpoints for LiveEndpoints {
    fn prepare(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        target: &rc2fx_core::Target,
        mode: rc2fx_native_control::PathMode,
        reset: rc2fx_native_control::ResetPolicy,
    ) -> Result<parameters::Staged, rc2fx_native_control::ErrorCode> {
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            control
                .prepare_with(target, transaction.revision, mode, reset)
                .map_err(parameter_error)
        })
    }
    fn commit(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        staged: &mut Option<parameters::Staged>,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        // Publications that replace processing first fade the running audio out
        // (~10 ms of callbacks) without holding registry or parameter locks, so
        // port lifecycle and status reads never stall. If the path closes
        // meanwhile, `handing` releases the old runtime on the control thread.
        let handing =
            with_parameter_control(transaction.context, &transaction.target_id, |control| {
                let staged = staged
                    .as_ref()
                    .ok_or(rc2fx_native_control::ErrorCode::Unavailable)?;
                control.hand_off(staged).map_err(parameter_error)
            })?;
        handing.wait();
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            let staged = staged
                .take()
                .ok_or(rc2fx_native_control::ErrorCode::Unavailable)?;
            control
                .reserve_after(staged, handing)
                .map_err(parameter_error)?
                .commit();
            Ok(())
        })
    }
    fn status(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<rc2fx_native_control::TargetStatus, rc2fx_native_control::ErrorCode> {
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            target_status(control)
        })
    }
}
impl ipc::Backend for NativeBackend {
    fn inventory(
        &mut self,
    ) -> Result<Vec<rc2fx_native_control::ContextStatus>, rc2fx_native_control::ErrorCode> {
        self.transactions.reap(std::time::Instant::now());
        let _registry = CONTROL.try_lock().map_err(lock_error)?;
        let mut inventory = Vec::new();
        // A retired context's last callback peak is reported once, with the
        // inventory that follows its retirement.
        let retired_recent = RETIRED_TIMING.take_callback_recent();
        for context in &CONTEXTS {
            if !context.slot.load(Ordering::Acquire).is_live() {
                continue;
            }
            let mut controls = context.parameters.try_lock().map_err(lock_error)?;
            let targets = controls
                .iter_mut()
                .map(target_status)
                .collect::<Result<Vec<_>, _>>()?;
            let recent = context.timing.take_callback_recent().max(retired_recent);
            inventory.push(rc2fx_native_control::ContextStatus {
                context: context.token.load(Ordering::Relaxed),
                targets,
                // Live context plus what already retired: the app's view stays
                // monotonic across rebuilds, which is what its fault counters
                // need to see.
                timing: Some(context.timing.merged_with(RETIRED_TIMING.snapshot())),
                recent_callback_wall_ns: (recent != 0).then_some(recent),
                counters: engine_counters(),
                events: engine_events(),
            });
        }
        Ok(inventory)
    }
    fn prepare(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        target: &rc2fx_core::Target,
        mode: rc2fx_native_control::PathMode,
        reset: rc2fx_native_control::ResetPolicy,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions.prepare_with(
            transaction,
            target,
            mode,
            reset,
            std::time::Instant::now(),
            &mut LiveEndpoints,
        )
    }
    fn commit(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions
            .commit(transaction, std::time::Instant::now(), &mut LiveEndpoints)
    }
    fn cancel(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions
            .cancel(transaction, std::time::Instant::now())
    }
}
pub(crate) fn start_control_server() {
    audit_report::start_report_refresh();
    let Some(path) = CONTROL_SOCKET.get() else {
        return;
    };
    SERVER.get_or_init(|| {
        Mutex::new(
            match ipc::Server::start(path, NativeBackend::default(), Some(&CONTROL_ADMISSION)) {
                Ok(server) => Some(server),
                Err(_) => {
                    SERVER_ERRORS.fetch_add(1, Ordering::Relaxed);
                    None
                }
            },
        )
    });
}
pub(crate) fn stop_control_server() -> bool {
    let Some(server) = SERVER.get() else {
        return true;
    };
    let Ok(mut server) = server.lock() else {
        return false;
    };
    server.as_mut().is_none_or(|server| server.shutdown())
}
