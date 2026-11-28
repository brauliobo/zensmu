//! Async-signal-safe SIGINT/SIGTERM handlers and thread stop bridge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub(super) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub(super) fn install_sigint_handler() {
    INTERRUPTED.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint as *const () as usize);
    }
}

pub(super) fn install_stop_signal_handlers() {
    INTERRUPTED.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint as *const () as usize);
        libc::signal(libc::SIGTERM, handle_sigint as *const () as usize);
    }
}

/// Bridges the async-signal-safe `INTERRUPTED` flag to a shared `Arc<AtomicBool>`
/// stop flag consumed by core library loops (`bench::run_core_cycler`, `metrics::serve_metrics`).
pub(super) struct SignalStopBridge {
    stop_flag: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl SignalStopBridge {
    pub(super) fn install() -> Self {
        install_stop_signal_handlers();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let t_stop = Arc::clone(&stop_flag);
        let t_done = Arc::clone(&done);
        let handle = thread::Builder::new()
            .name("zensmu-sig-bridge".into())
            .spawn(move || {
                while !t_done.load(Ordering::Acquire) {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        t_stop.store(true, Ordering::Release);
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            })
            .ok();
        Self {
            stop_flag,
            done,
            handle,
        }
    }

    pub(super) fn stop_flag(&self) -> &Arc<AtomicBool> {
        &self.stop_flag
    }
}

impl Drop for SignalStopBridge {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
