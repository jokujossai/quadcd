//! `SyncRunner`: one-shot and long-running sync orchestration.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Instant, SystemTime};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::cd_config::CDConfig;
use crate::config::Config;
use crate::install;

use super::image::{dedup_images, extract_images, ImagePuller, ImageRef};
use super::repo::{safe_repo_dir, sync_repo_inner, SyncResult, SyncStatus};
use super::systemd::SystemdTrait;
use super::units::{activate_changed_units_inner, all_unit_files};
use super::vcs::Vcs;

#[derive(Default)]
struct SyncOutcomeSummary {
    cloned: usize,
    updated: usize,
    up_to_date: usize,
    skipped: usize,
    failed: usize,
}

impl SyncOutcomeSummary {
    fn is_empty(&self) -> bool {
        self.cloned == 0
            && self.updated == 0
            && self.up_to_date == 0
            && self.skipped == 0
            && self.failed == 0
    }

    fn push_parts(&self, parts: &mut Vec<String>) {
        if self.cloned > 0 {
            parts.push(format!(
                "{} cloned{}",
                self.cloned,
                if self.cloned == 1 {
                    " repository"
                } else {
                    " repositories"
                }
            ));
        }
        if self.updated > 0 {
            parts.push(format!(
                "{} updated{}",
                self.updated,
                if self.updated == 1 {
                    " repository"
                } else {
                    " repositories"
                }
            ));
        }
        if self.up_to_date > 0 {
            parts.push(format!(
                "{} up to date{}",
                self.up_to_date,
                if self.up_to_date == 1 {
                    " repository"
                } else {
                    " repositories"
                }
            ));
        }
        if self.skipped > 0 {
            parts.push(format!(
                "{} skipped{}",
                self.skipped,
                if self.skipped == 1 {
                    " repository"
                } else {
                    " repositories"
                }
            ));
        }
        if self.failed > 0 {
            parts.push(format!(
                "{} failed{}",
                self.failed,
                if self.failed == 1 {
                    " repository"
                } else {
                    " repositories"
                }
            ));
        }
    }

    fn log(&self, cfg: &Config, prefix: &str) {
        if self.is_empty() {
            return;
        }

        let mut parts = Vec::new();
        self.push_parts(&mut parts);
        let _ = writeln!(cfg.output.err(), "[quadcd] {prefix}: {}", parts.join(", "));
    }
}

/// Holds shared context for sync operations and provides methods for one-shot
/// and long-running sync modes.
pub struct SyncRunner<'a> {
    pub(crate) cfg: &'a Config,
    pub(crate) vcs: &'a dyn Vcs,
    pub(crate) systemd: &'a dyn SystemdTrait,
    pub(crate) image_puller: &'a dyn ImagePuller,
    sync_only: bool,
}

impl<'a> SyncRunner<'a> {
    pub fn new(
        cfg: &'a Config,
        vcs: &'a dyn Vcs,
        systemd: &'a dyn SystemdTrait,
        image_puller: &'a dyn ImagePuller,
    ) -> Self {
        Self {
            cfg,
            vcs,
            systemd,
            image_puller,
            sync_only: false,
        }
    }

    /// Enable sync-only mode: pull changes but skip daemon-reload and restarts.
    pub fn sync_only(mut self, sync_only: bool) -> Self {
        self.sync_only = sync_only;
        self
    }

    /// Pre-pull container images for changed `.container` and `.image` files.
    ///
    /// Reads `Image=` lines from the source files in each repo directory,
    /// applies variable substitution, and pulls each unique image so that
    /// restarts don't incur image download time.
    pub(crate) fn pre_pull_images(&self, changed_files: &[String]) {
        let source_dirs = self.cfg.effective_source_dirs();
        let mut all_images: Vec<ImageRef> = Vec::new();

        for (source_dir, env_vars) in &source_dirs {
            all_images.extend(extract_images(
                changed_files,
                source_dir,
                env_vars,
                self.cfg.verbose,
                &self.cfg.output,
            ));
        }

        dedup_images(&mut all_images);

        for image in &all_images {
            self.image_puller.pull(image, self.cfg);
        }
    }

    /// Restart changed units using `self.systemd`.
    pub(crate) fn restart_changed_units(&self, changed_files: &[String]) {
        activate_changed_units_inner(self.systemd, changed_files, self.cfg);
    }

    /// Sync all configured repositories. Returns changed unit files and a
    /// count of repos that failed to sync.
    pub fn sync_all(&self, cd_config: &CDConfig) -> SyncResult {
        let mut all_changed = Vec::new();
        let mut failures: usize = 0;
        let mut summary = SyncOutcomeSummary::default();

        for (name, repo_config) in &cd_config.repositories {
            let repo_dir = match safe_repo_dir(&self.cfg.data_dir, name) {
                Ok(d) => d,
                Err(e) => {
                    summary.skipped += 1;
                    let _ = writeln!(self.cfg.output.err(), "[quadcd] {e}, skipping");
                    continue;
                }
            };
            if let Err(e) = fs::create_dir_all(&repo_dir) {
                summary.skipped += 1;
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: failed to create directory {}: {e}, skipping '{name}'",
                    repo_dir.display()
                );
                continue;
            }

            match sync_repo_inner(self.vcs, &repo_dir, repo_config, self.cfg) {
                Ok(SyncStatus::Cloned) => {
                    summary.cloned += 1;
                    let _ = writeln!(self.cfg.output.err(), "[quadcd] Cloned repository '{name}'");
                    all_changed.extend(all_unit_files(&repo_dir));
                }
                Ok(SyncStatus::Updated { changed_files }) => {
                    summary.updated += 1;
                    if !changed_files.is_empty() {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Updated repository '{name}' ({} unit(s) changed)",
                            changed_files.len()
                        );
                    } else {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Updated repository '{name}' (no unit files changed)"
                        );
                    }
                    all_changed.extend(changed_files);
                }
                Ok(SyncStatus::AlreadyUpToDate) => {
                    summary.up_to_date += 1;
                    if self.cfg.verbose {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Repository '{name}' is already up to date"
                        );
                    }
                }
                Err(e) => {
                    failures += 1;
                    summary.failed += 1;
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Error syncing repository '{name}': {e}"
                    );
                }
            }
        }

        summary.log(self.cfg, "Sync summary");

        SyncResult {
            changed_files: all_changed,
            failures,
        }
    }

    /// Reload units after a sync: daemon-reload, pre-pull images, restart units.
    ///
    /// No-op when `changed_files` is empty. In sync-only mode, logs changed
    /// files but skips daemon-reload and restarts.
    fn apply_changes(&self, changed_files: &[String]) {
        if changed_files.is_empty() {
            return;
        }
        let _ = writeln!(
            self.cfg.output.err(),
            "[quadcd] Changed units: {}",
            changed_files.join(", ")
        );
        if self.sync_only {
            return;
        }
        self.systemd.daemon_reload(self.cfg);
        self.pre_pull_images(changed_files);
        self.restart_changed_units(changed_files);
    }

    /// One-shot sync: sync all repos, daemon-reload, then restart changed units.
    pub fn run_once(&self, cd_config: &CDConfig) -> usize {
        let result = self.sync_all(cd_config);
        self.apply_changes(&result.changed_files);
        result.failures
    }

    /// Check whether a `notify` event targets the config file.
    ///
    /// Returns `true` only for `Modify` or `Create` events whose paths
    /// include `config_path`.  We watch the parent directory (to catch
    /// editor rename-replace patterns), so this filter is essential to
    /// ignore unrelated file changes in the same directory.
    pub(crate) fn is_config_event(event: &notify::Event, config_path: &Path) -> bool {
        matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_))
            && event.paths.iter().any(|p| p == config_path)
    }

    /// Compare the config file's current `mtime` against `last`.
    ///
    /// Returns `Some(new_mtime)` when the file has been modified since
    /// `last` (or when `last` is `None`), allowing the caller to trigger
    /// a reload.  Returns `None` when unchanged or on any I/O error.
    pub(crate) fn check_config_mtime(path: &Path, last: Option<SystemTime>) -> Option<SystemTime> {
        let mtime = fs::metadata(path).ok()?.modified().ok()?;
        if last.map_or(true, |old| mtime > old) {
            Some(mtime)
        } else {
            None
        }
    }

    /// Long-running service loop that syncs repos on interval and watches the
    /// config file for changes.
    ///
    /// Runs until the `shutdown` flag is set (e.g. by a signal handler).
    pub fn run_service(self, cd_config: CDConfig, shutdown: &AtomicBool) {
        // Initial sync — held under the sync lock, then released before the
        // main loop so manual `quadcd sync` invocations can run between ticks.
        match install::acquire_sync_lock(&self.cfg.data_dir) {
            Ok(lock) => {
                let result = self.sync_all(&cd_config);
                self.apply_changes(&result.changed_files);
                if result.failures > 0 {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] {} repository sync(s) failed on startup",
                        result.failures
                    );
                }
                drop(lock);
            }
            Err(e) => {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Failed to acquire sync lock for initial sync: {e}"
                );
            }
        }

        // Set up per-repo interval tracking
        let mut current_config = cd_config;
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();
        for name in current_config.repositories.keys() {
            last_sync.insert(name.clone(), now);
        }

        // Set up config file watcher
        let (tx, rx) = mpsc::channel();
        // Secondary channel: forwards log strings from the watcher thread so
        // we can print them on the main thread (Output uses RefCell, not Send).
        let (log_tx, log_rx) = mpsc::channel::<String>();
        let mut watcher: Option<Box<dyn Watcher>> = None;

        if let Some(ref cp) = self.cfg.config_path {
            let config_path_clone = cp.clone();
            let tx_clone = tx.clone();
            let log_tx_clone = log_tx.clone();
            let verbose = self.cfg.verbose;
            match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        if verbose {
                            let _ = log_tx_clone.send(format!(
                                "[quadcd] Notify event: {:?} paths={:?}",
                                event.kind, event.paths
                            ));
                        }
                        if Self::is_config_event(&event, &config_path_clone) {
                            let _ = tx_clone.send(());
                        }
                    }
                    Err(e) => {
                        let _ =
                            log_tx_clone.send(format!("[quadcd] Warning: file watcher error: {e}"));
                    }
                }
            }) {
                Ok(mut w) => {
                    let watch_dir = cp.parent().unwrap_or(Path::new("/"));
                    if let Err(e) = w.watch(watch_dir, RecursiveMode::NonRecursive) {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Warning: could not watch config directory: {e}"
                        );
                    } else if self.cfg.verbose {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Watching config file: {}",
                            cp.display()
                        );
                    }
                    watcher = Some(Box::new(w));
                }
                Err(e) => {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Warning: could not create file watcher: {e}"
                    );
                }
            }
        }

        // Seed mtime for polling fallback (used when watcher failed)
        let mut config_mtime: Option<SystemTime> = self
            .cfg
            .config_path
            .as_ref()
            .and_then(|p| fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());

        // Main loop — tick is the smallest configured interval
        let mut tick = current_config.min_interval();
        let mut consecutive_skips: u32 = 0;
        while !shutdown.load(Ordering::Relaxed) {
            // Polling fallback: check mtime when watcher is unavailable
            if watcher.is_none() {
                if let Some(ref cp) = self.cfg.config_path {
                    if let Some(new_mtime) = Self::check_config_mtime(cp, config_mtime) {
                        config_mtime = Some(new_mtime);
                        let _ = tx.send(());
                    }
                }
            }

            // Drain log messages forwarded from the watcher thread
            while let Ok(msg) = log_rx.try_recv() {
                let _ = writeln!(self.cfg.output.err(), "{msg}");
            }

            // Acquire the sync lock only for the active sync phase so that
            // manual `quadcd sync` invocations can run between ticks.
            let failures = self.try_acquire_and_tick(
                &rx,
                &mut current_config,
                &mut last_sync,
                &mut tick,
                shutdown,
                &mut consecutive_skips,
            );
            if failures > 0 {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] {failures} repository sync(s) failed this tick"
                );
            }

            // Interruptible sleep: wake early on SIGTERM or a config-file change.
            // Using recv_timeout instead of thread::sleep means a notify event
            // breaks the sleep immediately rather than waiting for the full tick.
            let sleep_step = std::time::Duration::from_millis(200);
            let mut remaining = tick;
            while remaining > std::time::Duration::ZERO {
                let nap = remaining.min(sleep_step);
                match rx.recv_timeout(nap) {
                    Ok(()) => {
                        // Config changed during sleep — drain duplicates and
                        // re-queue one notification so service_tick sees it.
                        while rx.try_recv().is_ok() {}
                        let _ = tx.send(());
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                // Drain watcher log messages each step so they appear promptly.
                while let Ok(msg) = log_rx.try_recv() {
                    let _ = writeln!(self.cfg.output.err(), "{msg}");
                }
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                remaining = remaining.saturating_sub(nap);
            }
        }

        let _ = writeln!(self.cfg.output.err(), "[quadcd] Shutting down");
    }

    /// Attempt to reload the config file when a change notification is received.
    ///
    /// Drains duplicate notifications from the channel, reads the new config,
    /// warns about URL changes (unless `--force`), updates `last_sync` for new
    /// repos, and syncs all repos if the reload succeeds.
    ///
    /// Returns `Some(new_config)` on successful reload, `None` otherwise
    /// (the caller keeps the current config).
    pub(crate) fn try_reload_config(
        &self,
        rx: &mpsc::Receiver<()>,
        current_config: &CDConfig,
        last_sync: &mut HashMap<String, Instant>,
    ) -> Option<CDConfig> {
        // Drain any extra notifications
        while rx.try_recv().is_ok() {}

        let _ = writeln!(
            self.cfg.output.err(),
            "[quadcd] Config file changed, reloading"
        );
        let reload_result = self
            .cfg
            .config_path
            .as_ref()
            .map(|p| CDConfig::load_from_path(p));
        match reload_result {
            Some(Ok(new_config)) => {
                if self.cfg.verbose {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Config loaded, {} repo(s)",
                        new_config.repositories.len()
                    );
                }
                if !self.cfg.force {
                    for (name, new_repo) in &new_config.repositories {
                        if let Some(old_repo) = current_config.repositories.get(name) {
                            if old_repo.url != new_repo.url {
                                let _ = writeln!(
                                    self.cfg.output.err(),
                                    "[quadcd] Warning: URL changed for '{}': '{}' -> '{}'. Use --force to apply.",
                                    name, old_repo.url, new_repo.url
                                );
                            }
                        }
                    }
                }

                let now = Instant::now();
                for name in new_config.repositories.keys() {
                    last_sync.entry(name.clone()).or_insert(now);
                }

                if self.cfg.verbose {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Syncing {} repository(ies) after config reload",
                        new_config.repositories.len()
                    );
                }
                let result = self.sync_all(&new_config);
                self.apply_changes(&result.changed_files);
                if self.cfg.verbose {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Config reload sync complete ({} changed, {} failed)",
                        result.changed_files.len(),
                        result.failures
                    );
                }
                if result.failures > 0 {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] {} repository sync(s) failed after config reload",
                        result.failures
                    );
                }

                Some(new_config)
            }
            Some(Err(e)) => {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: {e}, keeping current config"
                );
                None
            }
            None => {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: config path not set, cannot reload"
                );
                None
            }
        }
    }

    /// Try to acquire the sync lock and run one `service_tick`. If the lock
    /// is held by another process (typically a manual `quadcd sync`), the
    /// tick is skipped and `consecutive_skips` is incremented; a log line is
    /// emitted every skip so operators can see how long the service has
    /// deferred. `consecutive_skips` is reset to 0 on successful acquire.
    ///
    /// Returns the number of repositories that failed to sync this tick
    /// (always 0 when the tick was skipped).
    pub(crate) fn try_acquire_and_tick(
        &self,
        rx: &mpsc::Receiver<()>,
        current_config: &mut CDConfig,
        last_sync: &mut HashMap<String, Instant>,
        tick: &mut std::time::Duration,
        shutdown: &AtomicBool,
        consecutive_skips: &mut u32,
    ) -> usize {
        match install::try_acquire_sync_lock(&self.cfg.data_dir) {
            Ok(Some(lock)) => {
                *consecutive_skips = 0;
                let failures = self.service_tick(rx, current_config, last_sync, tick, shutdown);
                drop(lock);
                failures
            }
            Ok(None) => {
                *consecutive_skips = consecutive_skips.saturating_add(1);
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Service sync skipped ({n} consecutive skip(s) since last successful sync) — lock held by another quadcd process",
                    n = *consecutive_skips
                );
                0
            }
            Err(e) => {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Failed to acquire sync lock this tick: {e}"
                );
                0
            }
        }
    }

    /// Execute one iteration of the service loop.
    ///
    /// Checks the channel for config-change notifications, reloads if needed,
    /// then syncs any repos whose interval has elapsed. Mutates `current_config`,
    /// `last_sync`, and `tick` as appropriate.
    /// Returns the number of repositories that failed to sync this tick.
    pub(crate) fn service_tick(
        &self,
        rx: &mpsc::Receiver<()>,
        current_config: &mut CDConfig,
        last_sync: &mut HashMap<String, Instant>,
        tick: &mut std::time::Duration,
        shutdown: &AtomicBool,
    ) -> usize {
        let config_changed = rx.try_recv().is_ok();
        if config_changed {
            if let Some(new_config) = self.try_reload_config(rx, current_config, last_sync) {
                *current_config = new_config;
                *tick = current_config.min_interval();
            }
        }

        // Check per-repo intervals
        let now = Instant::now();
        let mut interval_changed: Vec<String> = Vec::new();
        let mut failures: usize = 0;
        let mut summary = SyncOutcomeSummary::default();
        for (name, repo_config) in &current_config.repositories {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            if let Some(interval) = repo_config.interval_duration() {
                let last = last_sync.get(name).copied().unwrap_or(now);
                if now.duration_since(last) >= interval {
                    let repo_dir = match safe_repo_dir(&self.cfg.data_dir, name) {
                        Ok(d) => d,
                        Err(e) => {
                            summary.skipped += 1;
                            let _ = writeln!(self.cfg.output.err(), "[quadcd] {e}, skipping");
                            continue;
                        }
                    };
                    if let Err(e) = fs::create_dir_all(&repo_dir) {
                        summary.skipped += 1;
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Warning: failed to create directory {}: {e}, skipping '{name}'",
                            repo_dir.display()
                        );
                        continue;
                    }
                    match sync_repo_inner(self.vcs, &repo_dir, repo_config, self.cfg) {
                        Ok(SyncStatus::Cloned) => {
                            summary.cloned += 1;
                            let _ = writeln!(
                                self.cfg.output.err(),
                                "[quadcd] Cloned repository '{name}'"
                            );
                            interval_changed.extend(all_unit_files(&repo_dir));
                        }
                        Ok(SyncStatus::Updated { changed_files }) => {
                            summary.updated += 1;
                            if !changed_files.is_empty() {
                                let _ = writeln!(
                                    self.cfg.output.err(),
                                    "[quadcd] Synced repository '{name}' ({} unit(s) changed)",
                                    changed_files.len()
                                );
                            }
                            interval_changed.extend(changed_files);
                        }
                        Ok(SyncStatus::AlreadyUpToDate) => {
                            summary.up_to_date += 1;
                            if self.cfg.verbose {
                                let _ = writeln!(
                                    self.cfg.output.err(),
                                    "[quadcd] Repository '{name}' is up to date"
                                );
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            summary.failed += 1;
                            let _ = writeln!(
                                self.cfg.output.err(),
                                "[quadcd] Error syncing '{name}': {e}"
                            );
                        }
                    }
                    last_sync.insert(name.clone(), now);
                }
            }
        }

        self.apply_changes(&interval_changed);
        summary.log(self.cfg, "Service tick summary");
        failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cd_config::RepoConfig;
    use crate::config::test_config;
    use notify::EventKind;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::super::image::testing::MockImagePuller;
    use super::super::systemd::testing::MockSystemd;
    use super::super::vcs::testing::MockVcs;

    // service_tick

    #[test]
    fn service_tick_initial_sync_clones_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        // Create repo dir without .git so it will clone
        let repo_dir = tmp.path().join("myrepo");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(repo_dir.join("app.container"), "").unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1s".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        // Set last_sync far in the past so the interval has elapsed,
        // triggering a sync (which will clone since there's no .git dir)
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert(
            "myrepo".to_string(),
            Instant::now() - std::time::Duration::from_secs(60),
        );
        let mut tick = cd_config.min_interval();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        assert!(!vcs.clone_called.borrow().is_empty());
        assert!(last_sync.contains_key("myrepo"));
    }

    #[test]
    fn service_tick_interval_triggers_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
        *vcs.changed_files_val.borrow_mut() = vec!["app.container".to_string()];
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        let repo_dir = tmp.path().join("myrepo");
        fs::create_dir_all(repo_dir.join(".git")).unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1s".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        // Set last_sync far in the past so the interval has elapsed
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert(
            "myrepo".to_string(),
            Instant::now() - std::time::Duration::from_secs(60),
        );
        let mut tick = cd_config.min_interval();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        assert!(!vcs.pull_called.borrow().is_empty());
        assert!(*systemd.reload_called.borrow());
        assert!(err_buf
            .captured()
            .contains("Service tick summary: 1 updated repository"));
    }

    #[test]
    fn service_tick_interval_not_yet_due() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        let repo_dir = tmp.path().join("myrepo");
        fs::create_dir_all(repo_dir.join(".git")).unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1h".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        // Set last_sync to now — interval hasn't elapsed
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        // No sync should have been triggered
        assert!(vcs.pull_called.borrow().is_empty());
        assert!(vcs.clone_called.borrow().is_empty());
        assert!(!*systemd.reload_called.borrow());
    }

    #[test]
    fn service_tick_config_reload_adds_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        // Write an updated config file with an additional repo
        let config_file = tmp.path().join("quadcd.toml");
        fs::write(
            &config_file,
            r#"
[repositories.myrepo]
url = "https://example.com/repo.git"
interval = "1h"

[repositories.newrepo]
url = "https://example.com/new.git"
interval = "30s"
"#,
        )
        .unwrap();
        cfg.config_path = Some(config_file);

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        // Pre-create both repo dirs without .git so they will be cloned
        let repo1 = tmp.path().join("myrepo");
        fs::create_dir_all(&repo1).unwrap();
        let repo2 = tmp.path().join("newrepo");
        fs::create_dir_all(&repo2).unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1h".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();

        // Simulate config file change notification
        tx.send(()).unwrap();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        // The config should now contain both repos
        assert!(cd_config.repositories.contains_key("newrepo"));
        assert!(cd_config.repositories.contains_key("myrepo"));
        assert!(last_sync.contains_key("newrepo"));
        // tick should have updated to 30s (smallest interval)
        assert_eq!(tick, std::time::Duration::from_secs(30));
    }

    #[test]
    fn service_tick_config_reload_invalid_keeps_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        // Write an invalid config file
        let config_file = tmp.path().join("quadcd.toml");
        fs::write(&config_file, "this is not valid toml [[[").unwrap();
        cfg.config_path = Some(config_file);

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("5m".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();

        // Simulate config file change notification
        tx.send(()).unwrap();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        // Config should be unchanged — still has only "myrepo"
        assert_eq!(cd_config.repositories.len(), 1);
        assert!(cd_config.repositories.contains_key("myrepo"));
    }

    #[test]
    fn try_reload_config_without_config_path_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();
        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        let current_config = CDConfig {
            repositories: HashMap::new(),
        };

        let result = runner.try_reload_config(&rx, &current_config, &mut last_sync);

        assert!(result.is_none());
        assert!(err_buf
            .captured()
            .contains("Warning: config path not set, cannot reload"));
    }

    #[test]
    fn try_reload_config_logs_sync_failures_after_successful_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let config_file = tmp.path().join("quadcd.toml");
        fs::write(
            &config_file,
            r#"
[repositories.myrepo]
url = "https://example.com/new-url.git"
interval = "5m"

[repositories.newrepo]
url = "https://example.com/new-repo.git"
interval = "1h"
"#,
        )
        .unwrap();
        cfg.config_path = Some(config_file);

        let vcs = MockVcs::new();
        *vcs.remote_url_val.borrow_mut() = Err("connection refused".to_string());
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();
        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        fs::create_dir_all(tmp.path().join("myrepo").join(".git")).unwrap();

        let (_tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/old-url.git".to_string(),
                branch: None,
                interval: Some("5m".to_string()),
            },
        );
        let current_config = CDConfig {
            repositories: repos,
        };

        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());

        let result = runner.try_reload_config(&rx, &current_config, &mut last_sync);

        assert!(result.is_some());
        assert!(last_sync.contains_key("newrepo"));

        let stderr = err_buf.captured();
        assert!(stderr.contains("Config file changed, reloading"));
        assert!(stderr.contains("repository sync(s) failed after config reload"));
        assert!(stderr.contains("Error syncing repository 'myrepo'"));
        assert!(stderr.contains("connection refused"));
    }

    #[test]
    fn service_tick_url_change_warns_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();
        cfg.force = false;

        // Write config with a different URL for "myrepo"
        let config_file = tmp.path().join("quadcd.toml");
        fs::write(
            &config_file,
            r#"
[repositories.myrepo]
url = "https://example.com/new-url.git"
interval = "5m"
"#,
        )
        .unwrap();
        cfg.config_path = Some(config_file);

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        // Pre-create repo with .git so sync_repo_inner won't clone
        let repo_dir = tmp.path().join("myrepo");
        fs::create_dir_all(repo_dir.join(".git")).unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/old-url.git".to_string(),
                branch: None,
                interval: Some("5m".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();

        // Simulate config file change notification
        tx.send(()).unwrap();

        let shutdown = AtomicBool::new(false);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        let stderr = err_buf.captured();
        assert!(stderr.contains("Warning: URL changed for 'myrepo'"));
        assert!(stderr.contains("old-url.git"));
        assert!(stderr.contains("new-url.git"));
        assert!(stderr.contains("Use --force to apply"));
    }

    #[test]
    fn service_tick_repo_sync_error_logs_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        // remote_url error will cause sync to fail
        *vcs.remote_url_val.borrow_mut() = Err("connection refused".to_string());
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        // Pre-create repo dirs with .git so it tries to pull (not clone)
        let repo1 = tmp.path().join("failing-repo");
        fs::create_dir_all(repo1.join(".git")).unwrap();
        let repo2 = tmp.path().join("other-repo");
        fs::create_dir_all(repo2.join(".git")).unwrap();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut repos = HashMap::new();
        repos.insert(
            "failing-repo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1s".to_string()),
            },
        );
        repos.insert(
            "other-repo".to_string(),
            RepoConfig {
                url: "https://example.com/other.git".to_string(),
                branch: None,
                interval: Some("1s".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        // Set last_sync far in the past for both
        let past = Instant::now() - std::time::Duration::from_secs(60);
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("failing-repo".to_string(), past);
        last_sync.insert("other-repo".to_string(), past);
        let mut tick = cd_config.min_interval();

        let shutdown = AtomicBool::new(false);
        let failures =
            runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        let stderr = err_buf.captured();
        // Both repos should have been attempted (errors logged, not panicked)
        assert!(stderr.contains("Error syncing"));
        assert!(stderr.contains("connection refused"));
        assert!(stderr.contains("Service tick summary: 2 failed repositories"));
        // Both last_sync entries should have been updated
        assert!(last_sync.get("failing-repo").unwrap() > &past);
        assert!(last_sync.get("other-repo").unwrap() > &past);
        // Both syncs failed — failure count must match
        assert_eq!(failures, 2);
    }

    #[test]
    fn service_loop_shutdown_flag_terminates_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        *vcs.head_sha_val.borrow_mut() = Some("same".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("same".to_string());
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        let repo_dir = tmp.path().join("myrepo");
        fs::create_dir_all(repo_dir.join(".git")).unwrap();

        let cd_config = CDConfig {
            repositories: HashMap::new(),
        };

        // Set shutdown flag before starting
        let shutdown = AtomicBool::new(true);

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        // This should return immediately because shutdown is already set
        runner.run_service(cd_config, &shutdown);

        // If we reach here, the loop terminated correctly
    }

    // shutdown

    #[test]
    fn shutdown_flag_skips_remaining_repos_in_service_tick() {
        let tmp = tempfile::tempdir().unwrap();
        let stderr_buf: Vec<u8> = Vec::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(stderr_buf));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        *vcs.head_sha_val.borrow_mut() = Some("aaa".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("aaa".to_string());
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        // Create two repos with short intervals
        let mut repos = HashMap::new();
        repos.insert(
            "repo-a".to_string(),
            RepoConfig {
                url: "https://example.com/a.git".to_string(),
                branch: Some("main".to_string()),
                interval: Some("60s".to_string()),
            },
        );
        repos.insert(
            "repo-b".to_string(),
            RepoConfig {
                url: "https://example.com/b.git".to_string(),
                branch: Some("main".to_string()),
                interval: Some("60s".to_string()),
            },
        );
        let mut cd_config = CDConfig {
            repositories: repos,
        };

        // Create .git dirs so syncs would attempt pulls
        for name in cd_config.repositories.keys() {
            fs::create_dir_all(tmp.path().join(name).join(".git")).unwrap();
        }

        // Set all repos as due for sync
        let past = Instant::now() - std::time::Duration::from_secs(120);
        let mut last_sync: HashMap<String, Instant> = cd_config
            .repositories
            .keys()
            .map(|n| (n.clone(), past))
            .collect();

        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        let mut tick = cd_config.min_interval();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        // Shutdown is already set — per-repo loop should break immediately
        let shutdown = AtomicBool::new(true);
        runner.service_tick(&rx, &mut cd_config, &mut last_sync, &mut tick, &shutdown);

        // No repos should have been synced (pull not called)
        assert!(vcs.pull_called.borrow().is_empty());
    }

    #[test]
    fn service_loop_logs_shutdown_message() {
        use crate::output::tests::TestWriter;

        let tmp = tempfile::tempdir().unwrap();
        let err_buf = TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();

        let cd_config = CDConfig {
            repositories: HashMap::new(),
        };

        let shutdown = AtomicBool::new(true);

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        runner.run_service(cd_config, &shutdown);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("Shutting down"),
            "Expected shutdown message in stderr: {stderr}"
        );
    }

    // is_config_event

    #[test]
    fn is_config_event_matching_path() {
        let path = PathBuf::from("/etc/quadcd.toml");
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths: vec![path.clone()],
            attrs: Default::default(),
        };
        assert!(SyncRunner::is_config_event(&event, &path));
    }

    #[test]
    fn is_config_event_wrong_path() {
        let config = PathBuf::from("/etc/quadcd.toml");
        let other = PathBuf::from("/etc/other.conf");
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths: vec![other],
            attrs: Default::default(),
        };
        assert!(!SyncRunner::is_config_event(&event, &config));
    }

    #[test]
    fn is_config_event_wrong_kind() {
        let path = PathBuf::from("/etc/quadcd.toml");
        let event = notify::Event {
            kind: EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![path.clone()],
            attrs: Default::default(),
        };
        assert!(!SyncRunner::is_config_event(&event, &path));
    }

    #[test]
    fn is_config_event_create_kind() {
        let path = PathBuf::from("/etc/quadcd.toml");
        let event = notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![path.clone()],
            attrs: Default::default(),
        };
        assert!(SyncRunner::is_config_event(&event, &path));
    }

    #[test]
    fn check_config_mtime_detects_change() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("test.toml");
        fs::write(&file, "v1").unwrap();

        // First call with None should always return Some
        let mtime1 = SyncRunner::check_config_mtime(&file, None);
        assert!(mtime1.is_some());

        // Same mtime should return None
        assert!(SyncRunner::check_config_mtime(&file, mtime1).is_none());

        // Bump mtime by rewriting
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&file, "v2").unwrap();

        let mtime2 = SyncRunner::check_config_mtime(&file, mtime1);
        assert!(mtime2.is_some());
        assert!(mtime2.unwrap() > mtime1.unwrap());
    }

    #[test]
    fn check_config_mtime_missing_file() {
        let result = SyncRunner::check_config_mtime(Path::new("/no/such/file"), None);
        assert!(result.is_none());
    }

    // try_acquire_and_tick

    fn idle_cd_config() -> CDConfig {
        let mut repos = HashMap::new();
        // Long interval ensures service_tick performs no sync work — we're
        // only exercising the lock-acquire-and-dispatch path here.
        repos.insert(
            "myrepo".to_string(),
            RepoConfig {
                url: "https://example.com/repo.git".to_string(),
                branch: None,
                interval: Some("1h".to_string()),
            },
        );
        CDConfig {
            repositories: repos,
        }
    }

    #[test]
    fn try_acquire_and_tick_resets_counter_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();
        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut cd_config = idle_cd_config();
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();
        let shutdown = AtomicBool::new(false);
        let mut skips: u32 = 7;

        let failures = runner.try_acquire_and_tick(
            &rx,
            &mut cd_config,
            &mut last_sync,
            &mut tick,
            &shutdown,
            &mut skips,
        );

        assert_eq!(failures, 0);
        assert_eq!(skips, 0, "counter should reset when the lock is acquired");
    }

    #[test]
    fn try_acquire_and_tick_skips_and_logs_when_lock_held() {
        let tmp = tempfile::tempdir().unwrap();
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();
        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        // Hold the sync lock externally to simulate a concurrent manual sync.
        let _held = crate::install::acquire_sync_lock(&cfg.data_dir).unwrap();

        let (_tx, rx) = mpsc::channel::<()>();
        let mut cd_config = idle_cd_config();
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();
        let shutdown = AtomicBool::new(false);
        let mut skips: u32 = 0;

        let failures = runner.try_acquire_and_tick(
            &rx,
            &mut cd_config,
            &mut last_sync,
            &mut tick,
            &shutdown,
            &mut skips,
        );
        assert_eq!(failures, 0);
        assert_eq!(skips, 1);

        // Second contended call should increment the counter again and log.
        let failures = runner.try_acquire_and_tick(
            &rx,
            &mut cd_config,
            &mut last_sync,
            &mut tick,
            &shutdown,
            &mut skips,
        );
        assert_eq!(failures, 0);
        assert_eq!(skips, 2);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("Service sync skipped (1 consecutive skip(s)")
                && stderr.contains("Service sync skipped (2 consecutive skip(s)")
                && stderr.contains("lock held by another quadcd process"),
            "expected skip log with counter, got: {stderr}"
        );

        // No sync work should have been performed while the lock was held.
        assert!(vcs.pull_called.borrow().is_empty());
        assert!(vcs.clone_called.borrow().is_empty());
        assert!(!*systemd.reload_called.borrow());
    }

    #[test]
    fn try_acquire_and_tick_resets_counter_after_contention() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let image_puller = MockImagePuller::new();
        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

        let (_tx, rx) = mpsc::channel::<()>();
        let mut cd_config = idle_cd_config();
        let mut last_sync: HashMap<String, Instant> = HashMap::new();
        last_sync.insert("myrepo".to_string(), Instant::now());
        let mut tick = cd_config.min_interval();
        let shutdown = AtomicBool::new(false);
        let mut skips: u32 = 0;

        // Contended tick.
        {
            let _held = crate::install::acquire_sync_lock(&cfg.data_dir).unwrap();
            runner.try_acquire_and_tick(
                &rx,
                &mut cd_config,
                &mut last_sync,
                &mut tick,
                &shutdown,
                &mut skips,
            );
        }
        assert_eq!(skips, 1);

        // Lock is now free — next call should acquire and reset the counter.
        runner.try_acquire_and_tick(
            &rx,
            &mut cd_config,
            &mut last_sync,
            &mut tick,
            &shutdown,
            &mut skips,
        );
        assert_eq!(skips, 0);
    }
}
