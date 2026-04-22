//! QuadCD — a systemd generator and continuous deployment tool for Quadlet and
//! systemd unit files.
//!
//! # Subcommands
//!
//! - `quadcd generate [flags] normal-dir [early-dir] [late-dir]` — systemd generator mode.
//! - `quadcd sync [--service] [--force] [--user] [-v]` — git-based sync mode.
//! - `quadcd version` — print version and exit.
//! - `quadcd help` — print usage and exit.
//!
//! # Generator auto-detection
//!
//! If all three conditions are met, generator mode is entered automatically:
//! 1. Argument count (excluding argv\[0\]) is 1 or 3
//! 2. `SYSTEMD_SCOPE` environment variable is set
//! 3. First argument is an existing directory

pub mod cd_config;
pub(crate) mod cli;
pub mod config;
pub(crate) mod dryrun;
pub mod install;
pub mod output;
pub mod sync;

mod app;
mod generator;

use std::sync::atomic::{AtomicBool, Ordering};

pub use app::App;
pub use config::Config;
pub use generator::{Generator, GeneratorImpl};

/// Re-exports of `pub(crate)` items for use in integration tests.
#[cfg(feature = "test-support")]
pub mod testing {
    pub use crate::dryrun::DryRunner;
    pub use crate::sync::testing::{MockImagePuller, MockSystemd, MockVcs};
}

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Global shutdown flag set by signal handler.
pub(crate) static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Install SIGTERM and SIGINT handlers that set the global `SHUTDOWN` flag.
///
/// The handler only performs an async-signal-safe atomic store.
pub(crate) fn install_signal_handlers() {
    extern "C" fn handler(_sig: libc::c_int) {
        SHUTDOWN.store(true, Ordering::Relaxed);
    }
    unsafe {
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    /// Serialize signal tests: `SHUTDOWN` is a global, and tests run in
    /// parallel by default.  Any test that reads or writes `SHUTDOWN` must
    /// acquire this lock so it doesn't race other signal tests.
    static SIG_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that resets `SHUTDOWN` to `false` on drop, even if the
    /// test body panics, so subsequent tests start with a clean flag.
    struct ResetShutdown;
    impl Drop for ResetShutdown {
        fn drop(&mut self) {
            SHUTDOWN.store(false, Ordering::Relaxed);
        }
    }

    #[test]
    fn sigterm_sets_shutdown_flag() {
        let _lock = SIG_LOCK.lock().unwrap();
        SHUTDOWN.store(false, Ordering::Relaxed);
        install_signal_handlers();
        let _reset = ResetShutdown;
        unsafe { libc::raise(libc::SIGTERM) };
        assert!(SHUTDOWN.load(Ordering::Relaxed));
    }

    #[test]
    fn sigint_sets_shutdown_flag() {
        let _lock = SIG_LOCK.lock().unwrap();
        SHUTDOWN.store(false, Ordering::Relaxed);
        install_signal_handlers();
        let _reset = ResetShutdown;
        unsafe { libc::raise(libc::SIGINT) };
        assert!(SHUTDOWN.load(Ordering::Relaxed));
    }
}
