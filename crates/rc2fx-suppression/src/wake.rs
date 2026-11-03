//! Frame notification for the Linux device worker. The callback performs one
//! nonblocking futex wake after publishing a frame, never a wait or allocation.
//! An epoch captured before checking the queue closes the empty-to-wait race.
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Default)]
pub(crate) struct Wake {
    epoch: AtomicU32,
}

impl Wake {
    pub fn observe(&self) -> u32 {
        self.epoch.load(Ordering::Acquire)
    }

    pub fn notify(&self) -> bool {
        self.epoch.fetch_add(1, Ordering::Release);
        #[cfg(target_os = "linux")]
        {
            // SAFETY: AtomicU32 has the required four-byte alignment. Shared
            // owns this stable address until the worker has exited. WAKE never
            // blocks and does not dereference the unused timeout arguments.
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    self.epoch.as_ptr(),
                    libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                    1,
                    std::ptr::null::<libc::timespec>(),
                    std::ptr::null::<u32>(),
                    0,
                ) >= 0
            }
        }
        #[cfg(not(target_os = "linux"))]
        true
    }

    pub fn wait(&self, observed: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            // The timeout is a control-side failure escape, not the normal
            // audio wake cadence. Spurious wakes and signals recheck the queue.
            self.wait_for(observed, 100_000_000).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Desktop hosts do not run the product audio engine. Retain their
            // portable test/qualification behavior without a Linux syscall.
            let _ = observed;
            std::thread::sleep(std::time::Duration::from_micros(100));
            true
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for(&self, observed: u32, nanoseconds: libc::c_long) -> Result<Wait, ()> {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: nanoseconds,
        };
        // SAFETY: a live aligned atomic word and initialized relative timeout.
        // Kernel compares the epoch and starts waiting atomically; a publish
        // between observe/queue-pop/wait returns EAGAIN rather than losing work.
        let result = unsafe {
            libc::syscall(
                libc::SYS_futex,
                self.epoch.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                observed,
                &timeout,
                std::ptr::null::<u32>(),
                0,
            )
        };
        if result == 0 {
            return Ok(Wait::Woken);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EAGAIN) => Ok(Wait::Changed),
            Some(libc::EINTR) => Ok(Wait::Interrupted),
            Some(libc::ETIMEDOUT) => Ok(Wait::TimedOut),
            _ => Err(()),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum Wait {
    Woken,
    Changed,
    Interrupted,
    TimedOut,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn publish_between_empty_check_and_wait_cannot_be_lost() {
        let wake = Wake::default();
        let observed = wake.observe();
        assert!(wake.notify());
        assert_eq!(wake.wait_for(observed, 0), Ok(Wait::Changed));
        assert_eq!(wake.wait_for(wake.observe(), 0), Ok(Wait::TimedOut));
    }

    #[test]
    fn notifications_coalesce_and_wrapping_epoch_still_changes() {
        let wake = Wake {
            epoch: AtomicU32::new(u32::MAX - 1),
        };
        let observed = wake.observe();
        assert!(wake.notify());
        assert!(wake.notify());
        assert_eq!(wake.observe(), 0);
        assert_eq!(wake.wait_for(observed, 0), Ok(Wait::Changed));
    }

    #[test]
    fn notifications_publish_memory_across_real_worker_waits() {
        use std::sync::{Arc, atomic::AtomicBool};
        let wake = Arc::new(Wake::default());
        let finished = Arc::new(AtomicBool::new(false));
        let (ready, receive) = std::sync::mpsc::sync_channel(0);
        let (worker_wake, worker_finished) = (wake.clone(), finished.clone());
        let worker = std::thread::spawn(move || {
            let epoch = worker_wake.observe();
            ready.send(()).unwrap();
            loop {
                if worker_finished.load(Ordering::Acquire) {
                    break;
                }
                assert!(worker_wake.wait(epoch));
            }
        });
        receive.recv().unwrap();
        finished.store(true, Ordering::Release);
        assert!(wake.notify());
        worker.join().unwrap();
    }
}
