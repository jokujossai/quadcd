//! Git repository synchronisation for continuous deployment.
//!
//! Provides one-shot sync of individual repos and all configured repos, plus a
//! long-running service loop that watches the config file and syncs repos on
//! their configured intervals.
//!
//! After syncing, changed unit files are identified via `git diff` between the
//! pre- and post-sync commit SHAs. After `daemon-reload`, the corresponding
//! services are restarted with `systemctl restart`.

mod cmd;
mod image;
mod podman;
mod repo;
mod runner;
mod systemd;
mod units;
mod vcs;

pub use image::{ImagePuller, ImageRef};
pub use podman::Podman;
pub use repo::{SyncResult, SyncStatus};
pub use runner::SyncRunner;
pub use systemd::{Systemd, SystemdTrait};
pub use vcs::{GitVcs, Vcs};

pub(crate) use units::is_unit_file;

/// Re-exports of mock types for integration tests.
#[cfg(feature = "test-support")]
pub mod testing {
    pub use super::image::testing::MockImagePuller;
    pub use super::systemd::testing::MockSystemd;
    pub use super::vcs::testing::MockVcs;
}
