//! One process-wide stop request. Ctrl-C and, on Unix, SIGTERM and SIGHUP (terminal closed,
//! `kill`, a service manager); on Windows also Ctrl-Break, console close, logoff and shutdown.
//!
//! The transfer loops wait on [`requested`], flush their checkpoint and return. Once a handler
//! is installed the operating system no longer ends the process by itself, so [`install`] also
//! enforces a short grace period: a second request, or the grace running out while something
//! else (such as hashing a large file) is still busy, ends the process with exit code 130.

use std::{sync::OnceLock, time::Duration};
use tokio::sync::watch;

/// Time given to flush and unwind after the first request.
const GRACE: Duration = Duration::from_secs(3);

fn channel() -> &'static watch::Sender<bool> {
    static CHANNEL: OnceLock<watch::Sender<bool>> = OnceLock::new();
    CHANNEL.get_or_init(|| watch::channel(false).0)
}

/// Resolves once a stop has been requested; stays resolved afterwards.
pub async fn requested() {
    let mut receiver = channel().subscribe();
    let _ = receiver.wait_for(|stop| *stop).await;
}

/// Starts listening. Safe to call more than once; must run inside the Tokio runtime.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut signals = Signals::new();
        tokio::spawn(async move {
            signals.recv().await;
            channel().send_replace(true);
            tokio::select! {
                _ = signals.recv() => {}
                _ = tokio::time::sleep(GRACE) => {}
            }
            crate::ui::restore_terminal();
            eprintln!("fluxget: interrupted");
            std::process::exit(130);
        });
    });
}

/// Waits for a signal stream, or forever when it could not be registered.
macro_rules! wait {
    ($signal:expr) => {
        async {
            match $signal.as_mut() {
                Some(signal) => {
                    signal.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        }
    };
}

#[cfg(unix)]
struct Signals {
    interrupt: Option<tokio::signal::unix::Signal>,
    terminate: Option<tokio::signal::unix::Signal>,
    hangup: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl Signals {
    fn new() -> Self {
        use tokio::signal::unix::{signal, SignalKind};
        Self {
            interrupt: signal(SignalKind::interrupt()).ok(),
            terminate: signal(SignalKind::terminate()).ok(),
            hangup: signal(SignalKind::hangup()).ok(),
        }
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = wait!(self.interrupt) => {}
            _ = wait!(self.terminate) => {}
            _ = wait!(self.hangup) => {}
        }
    }
}

#[cfg(windows)]
struct Signals {
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
    brk: Option<tokio::signal::windows::CtrlBreak>,
    close: Option<tokio::signal::windows::CtrlClose>,
    logoff: Option<tokio::signal::windows::CtrlLogoff>,
    shutdown: Option<tokio::signal::windows::CtrlShutdown>,
}

#[cfg(windows)]
impl Signals {
    fn new() -> Self {
        use tokio::signal::windows;
        Self {
            ctrl_c: windows::ctrl_c().ok(),
            brk: windows::ctrl_break().ok(),
            close: windows::ctrl_close().ok(),
            logoff: windows::ctrl_logoff().ok(),
            shutdown: windows::ctrl_shutdown().ok(),
        }
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = wait!(self.ctrl_c) => {}
            _ = wait!(self.brk) => {}
            _ = wait!(self.close) => {}
            _ = wait!(self.logoff) => {}
            _ = wait!(self.shutdown) => {}
        }
    }
}
