use crate::{Error, WorkerOptions};
pub(crate) fn validate(options: WorkerOptions) -> Result<(), Error> {
    if options.cpu_mask == Some(0)
        || options
            .realtime_priority
            .is_some_and(|p| !(1..=20).contains(&p))
        || options.nice.is_some_and(|n| !(-20..=19).contains(&n))
        || (options.realtime_priority.is_some() && options.nice.is_some())
    {
        return Err(Error::Scheduling);
    }
    #[cfg(not(target_os = "linux"))]
    if options.cpu_mask.is_some() || options.realtime_priority.is_some() || options.nice.is_some() {
        return Err(Error::Scheduling);
    }
    Ok(())
}
#[cfg(target_os = "linux")]
pub(crate) fn apply(options: WorkerOptions) -> Result<(), Error> {
    apply_scope(options, false)
}
#[cfg(target_os = "linux")]
pub(crate) fn apply_kernel_allowed(options: WorkerOptions) -> Result<(), Error> {
    apply_scope(options, true)
}
#[cfg(target_os = "linux")]
fn apply_scope(options: WorkerOptions, kernel_allowed: bool) -> Result<(), Error> {
    validate(options)?;
    // SAFETY: all calls target this owned worker; valid stack structs, no
    // callback exists yet. Every requested affinity/policy is read back exactly.
    unsafe {
        let thread = libc::pthread_self();
        if let Some(mask) = options.cpu_mask {
            let mut inherited: libc::cpu_set_t = std::mem::zeroed();
            if libc::pthread_getaffinity_np(
                thread,
                std::mem::size_of_val(&inherited),
                &mut inherited,
            ) != 0
            {
                return Err(Error::Scheduling);
            }
            // A requested worker mask may narrow the creator's allowed mask,
            // never widen it merely because the enclosing cpuset permits more.
            if !kernel_allowed {
                for cpu in 0..64 {
                    if mask & (1 << cpu) != 0 && !libc::CPU_ISSET(cpu, &inherited) {
                        return Err(Error::Scheduling);
                    }
                }
            }
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            for cpu in 0..64 {
                if mask & (1 << cpu) != 0 {
                    libc::CPU_SET(cpu, &mut set);
                }
            }
            if libc::pthread_setaffinity_np(thread, std::mem::size_of_val(&set), &set) != 0 {
                return Err(Error::Scheduling);
            }
            let mut actual: libc::cpu_set_t = std::mem::zeroed();
            if libc::pthread_getaffinity_np(thread, std::mem::size_of_val(&actual), &mut actual)
                != 0
            {
                return Err(Error::Scheduling);
            }
            for cpu in 0..libc::CPU_SETSIZE as usize {
                if libc::CPU_ISSET(cpu, &actual) != (cpu < 64 && mask & (1u64 << cpu) != 0) {
                    return Err(Error::Scheduling);
                }
            }
        }
        if let Some(priority) = options.realtime_priority {
            let policy = libc::SCHED_RR;
            let param = libc::sched_param {
                sched_priority: priority as i32,
            };
            if libc::pthread_setschedparam(thread, policy, &param) != 0 {
                return Err(Error::Scheduling);
            }
            let mut actual_policy = 0;
            let mut actual_param = libc::sched_param { sched_priority: 0 };
            if libc::pthread_getschedparam(thread, &mut actual_policy, &mut actual_param) != 0
                || actual_policy != policy
                || actual_param.sched_priority != param.sched_priority
            {
                return Err(Error::Scheduling);
            }
        }
        if let Some(nice) = options.nice {
            // Ordinary scheduling, whatever the creator's class, at this nice.
            let param = libc::sched_param { sched_priority: 0 };
            if libc::pthread_setschedparam(thread, libc::SCHED_OTHER, &param) != 0 {
                return Err(Error::Scheduling);
            }
            // The raw call: glibc only has gettid() from 2.30, and the checks
            // build on 2.28.
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            if libc::setpriority(libc::PRIO_PROCESS, tid, nice as libc::c_int) != 0 {
                return Err(Error::Scheduling);
            }
            let mut actual_policy = 0;
            let mut actual_param = libc::sched_param { sched_priority: 0 };
            // getpriority returns -1 for nice -1 as well as for failure.
            *libc::__errno_location() = 0;
            let actual_nice = libc::getpriority(libc::PRIO_PROCESS, tid);
            if libc::pthread_getschedparam(thread, &mut actual_policy, &mut actual_param) != 0
                || actual_policy != libc::SCHED_OTHER
                || *libc::__errno_location() != 0
                || actual_nice != nice as libc::c_int
            {
                return Err(Error::Scheduling);
            }
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn apply(options: WorkerOptions) -> Result<(), Error> {
    validate(options)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn apply_kernel_allowed(options: WorkerOptions) -> Result<(), Error> {
    apply(options)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn explicit_kernel_scope_moves_the_worker_without_changing_its_creator() {
        std::thread::spawn(|| unsafe {
            let mut inherited: libc::cpu_set_t = std::mem::zeroed();
            let creator = libc::pthread_self();
            assert_eq!(
                libc::pthread_getaffinity_np(
                    creator,
                    std::mem::size_of_val(&inherited),
                    &mut inherited
                ),
                0
            );
            let cpus: Vec<_> = (0..64)
                .filter(|cpu| libc::CPU_ISSET(*cpu, &inherited))
                .collect();
            if cpus.len() < 2 {
                return;
            }
            let mut narrow: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut narrow);
            libc::CPU_SET(cpus[0], &mut narrow);
            assert_eq!(
                libc::pthread_setaffinity_np(creator, std::mem::size_of_val(&narrow), &narrow),
                0
            );
            let worker_cpu = cpus[1];
            std::thread::spawn(move || {
                apply_kernel_allowed(WorkerOptions {
                    cpu_mask: Some(1u64 << worker_cpu),
                    realtime_priority: None,
                    nice: None,
                })
                .unwrap();
            })
            .join()
            .unwrap();
            let mut after: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::pthread_getaffinity_np(creator, std::mem::size_of_val(&after), &mut after),
                0
            );
            for cpu in 0..libc::CPU_SETSIZE as usize {
                assert_eq!(libc::CPU_ISSET(cpu, &after), cpu == cpus[0]);
            }
        })
        .join()
        .unwrap();
    }
    #[test]
    fn cannot_widen_inherited_mask() {
        std::thread::spawn(|| unsafe {
            let thread = libc::pthread_self();
            let mut original: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::pthread_getaffinity_np(
                    thread,
                    std::mem::size_of_val(&original),
                    &mut original
                ),
                0
            );
            let cpus: Vec<_> = (0..64)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &original))
                .collect();
            if cpus.len() < 2 {
                return;
            }
            let mut narrow: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut narrow);
            libc::CPU_SET(cpus[0], &mut narrow);
            assert_eq!(
                libc::pthread_setaffinity_np(thread, std::mem::size_of_val(&narrow), &narrow),
                0
            );
            let options = WorkerOptions {
                cpu_mask: Some((1u64 << cpus[0]) | (1u64 << cpus[1])),
                realtime_priority: None,
                nice: None,
            };
            assert_eq!(apply(options), Err(Error::Scheduling));
            let mut after: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::pthread_getaffinity_np(thread, std::mem::size_of_val(&after), &mut after),
                0
            );
            for cpu in 0..libc::CPU_SETSIZE as usize {
                assert_eq!(libc::CPU_ISSET(cpu, &after), cpu == cpus[0]);
            }
            assert_eq!(
                apply(WorkerOptions {
                    cpu_mask: Some(1u64 << cpus[0]),
                    realtime_priority: None,
                    nice: None,
                }),
                Ok(())
            );
        })
        .join()
        .unwrap();
    }
    /// Raising nice needs no privilege, so this runs anywhere; it leaves the
    /// thread as SCHED_OTHER at the requested value.
    #[test]
    fn nice_requests_ordinary_scheduling_at_that_value() {
        std::thread::spawn(|| unsafe {
            assert_eq!(
                apply(WorkerOptions {
                    cpu_mask: None,
                    realtime_priority: None,
                    nice: Some(5),
                }),
                Ok(())
            );
            let mut policy = -1;
            let mut param = libc::sched_param { sched_priority: -1 };
            assert_eq!(
                libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param),
                0
            );
            assert_eq!(policy, libc::SCHED_OTHER);
            assert_eq!(
                libc::getpriority(
                    libc::PRIO_PROCESS,
                    libc::syscall(libc::SYS_gettid) as libc::id_t
                ),
                5
            );
        })
        .join()
        .unwrap();
    }
}
