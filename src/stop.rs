//! Co-operative stop flag shared between the driver threads and the host app.
//!
//! Mirrors the Python `threading.Event` usage (`addExit()` / `exit.wait(t)`):
//! `signal()` is the equivalent of `set()`, and `wait(timeout)` returns `true`
//! if the flag was set during (or before) the wait.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct StopFlag {
    flag: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<()>,
    rx: Arc<Mutex<mpsc::Receiver<()>>>,
}

impl StopFlag {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tx,
            rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// `addExit()` equivalent: request every waiter to stop.
    pub fn signal(&self) {
        use std::sync::atomic::Ordering;
        self.flag.store(true, Ordering::SeqCst);
        let _ = self.tx.send(());
    }

    pub fn is_set(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.flag.load(Ordering::SeqCst)
    }

    /// Sleeps for `timeout` (woken early by `signal`); returns `true` if the
    /// flag is set when the wait ends.
    pub fn wait(&self, timeout: Duration) -> bool {
        if self.is_set() {
            return true;
        }
        let _ = self.rx.lock().unwrap().recv_timeout(timeout);
        self.is_set()
    }
}

impl Default for StopFlag {
    fn default() -> Self {
        Self::new()
    }
}
