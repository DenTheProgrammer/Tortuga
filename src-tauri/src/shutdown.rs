//! Shutdown must not block the event loop, and its deadline must not depend on
//! Tokio, engine locks, disk I/O or Tauri's own teardown completing.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const GRACE_PERIOD: Duration = Duration::from_secs(2);
pub const HARD_DEADLINE: Duration = Duration::from_secs(4);

#[derive(Default)]
pub struct Shutdown {
    requested: AtomicBool,
    ready: AtomicBool,
}

impl Shutdown {
    pub fn begin(&self) -> bool {
        !self.requested.swap(true, Ordering::SeqCst)
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub fn finish(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }
}

/// Move even the synchronous prefix of cleanup off the UI / Tokio threads.
/// A timed-out blocking task cannot be cancelled; the watchdog remains armed.
pub async fn stop_within(work: impl FnOnce() + Send + 'static, grace: Duration) -> bool {
    matches!(
        tokio::time::timeout(grace, tokio::task::spawn_blocking(work)).await,
        Ok(Ok(()))
    )
}

pub fn arm_watchdog(deadline: Duration) {
    if std::thread::Builder::new()
        .name("shutdown-deadline".into())
        .spawn(move || {
            std::thread::sleep(deadline);
            force_exit();
        })
        .is_err()
    {
        force_exit();
    }
}

#[cfg(windows)]
fn force_exit() -> ! {
    // ExitProcess can itself deadlock in DLL_PROCESS_DETACH if another thread
    // owns a DLL lock. The emergency path must bypass those callbacks too.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn TerminateProcess(process: *mut std::ffi::c_void, exit_code: u32) -> i32;
    }
    // SAFETY: the pseudo-handle denotes this process only, and remains valid
    // until its termination. No user files or other processes are involved.
    unsafe { TerminateProcess(GetCurrentProcess(), 0) };
    std::process::abort()
}

#[cfg(not(windows))]
fn force_exit() -> ! {
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Command, time::Instant};

    #[test]
    fn concurrent_exit_requests_only_start_cleanup_once() {
        let state = std::sync::Arc::new(Shutdown::default());
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || usize::from(state.begin()))
            })
            .collect();
        assert_eq!(threads.into_iter().map(|t| t.join().unwrap()).sum::<usize>(), 1);
        assert!(state.is_requested());
        assert!(!state.is_ready());
        state.finish();
        assert!(state.is_ready());
        assert!(!state.begin());
    }

    #[test]
    fn blocking_cleanup_does_not_block_the_runtime_or_its_timeout() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(stop_within(|| {}, Duration::from_secs(1)).await);
            assert!(
                !stop_within(
                    || std::thread::sleep(Duration::from_millis(250)),
                    Duration::from_millis(25),
                )
                .await
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        });
        runtime.shutdown_background();
    }

    #[test]
    fn watchdog_terminates_a_process_with_a_blocked_event_loop() {
        let started = Instant::now();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shutdown::tests::blocked_shutdown_fixture",
                "--ignored",
            ])
            .env("TORTUGA_SHUTDOWN_FIXTURE", "1")
            .spawn()
            .unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("shutdown watchdog did not terminate the stuck process");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
    }

    #[test]
    #[ignore = "runs only in the watchdog's disposable subprocess"]
    fn blocked_shutdown_fixture() {
        if std::env::var("TORTUGA_SHUTDOWN_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        arm_watchdog(Duration::from_millis(250));
        // Neither this thread nor a Tokio timer can make progress.
        loop {
            std::thread::park();
        }
    }
}
