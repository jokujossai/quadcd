//! `SyncRunner`: one-shot and long-running sync orchestration.

mod service;

use std::collections::HashSet;
use std::fs;
use std::io::Write;

use crate::cd_config::CDConfig;
use crate::config::Config;

use super::image::{dedup_images, extract_images, ImagePuller, ImageRef};
use super::repo::{safe_repo_dir, sync_repo_inner, SyncResult, SyncStatus};
use super::settings::start_on_sync_units;
use super::systemd::SystemdTrait;
use super::units::{
    all_unit_files, execute_activation, plan_activation, stop_deleted_units_inner, ActivationPlan,
};
use super::vcs::{UnitChanges, Vcs};

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

    /// Pull each unique image of `files` (`.container`/`.image`, after variable
    /// substitution). Returns whether any pull was attempted, not whether it succeeded.
    pub(crate) fn pre_pull_images(&self, files: &[String]) -> bool {
        if files.is_empty() {
            return false;
        }

        let source_dirs = self.cfg.effective_source_dirs();
        let mut all_images: Vec<ImageRef> = Vec::new();

        for (source_dir, env_vars) in &source_dirs {
            all_images.extend(extract_images(
                files,
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

        !all_images.is_empty()
    }

    /// The image-bearing files among `changed_files` whose unit `plan` will
    /// leave running, i.e. the ones worth pre-pulling.
    fn files_worth_pulling(changed_files: &[String], plan: &ActivationPlan) -> Vec<String> {
        changed_files
            .iter()
            .filter(|f| Self::may_carry_image(f) && plan.activates_file(f))
            .cloned()
            .collect()
    }

    /// Same extension check as `extract_images`.
    fn may_carry_image(filename: &str) -> bool {
        filename.ends_with(".container") || filename.ends_with(".image")
    }

    /// Decide how the changed units should be activated. Must run after
    /// `daemon-reload` so systemd reports the updated unit files.
    pub(crate) fn plan_activation(
        &self,
        changed_files: &[String],
        start_on_sync: &HashSet<String>,
    ) -> ActivationPlan {
        plan_activation(self.systemd, changed_files, start_on_sync, self.cfg)
    }

    /// Start or restart the planned units using `self.systemd`. Returns the
    /// list of units that failed to reach an active/activating state.
    pub(crate) fn execute_activation(&self, plan: &ActivationPlan) -> Vec<String> {
        execute_activation(self.systemd, plan, self.cfg)
    }

    /// Must run before `daemon-reload`, while systemd still knows the units.
    pub(crate) fn stop_deleted_units(&self, deleted_files: &[String]) {
        stop_deleted_units_inner(self.systemd, deleted_files, self.cfg);
    }

    /// Sync all configured repositories. Returns changed unit files and a
    /// count of repos that failed to sync.
    pub fn sync_all(&self, cd_config: &CDConfig) -> SyncResult {
        let mut all_changes = UnitChanges::default();
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
                    all_changes.changed.extend(all_unit_files(&repo_dir));
                }
                Ok(SyncStatus::Updated { changes }) => {
                    summary.updated += 1;
                    if !changes.is_empty() {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Updated repository '{name}' ({} unit(s) changed, {} deleted)",
                            changes.changed.len(),
                            changes.deleted.len()
                        );
                    } else {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "[quadcd] Updated repository '{name}' (no unit files changed)"
                        );
                    }
                    all_changes.extend(changes);
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
            changes: all_changes,
            failures,
        }
    }

    /// Stop deleted units, `daemon-reload`, plan, pre-pull, activate.
    /// `--sync-only` only logs the changes.
    fn apply_changes(&self, changes: &UnitChanges) {
        if changes.is_empty() {
            return;
        }
        if !changes.changed.is_empty() {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Changed units: {}",
                changes.changed.join(", ")
            );
        }
        if !changes.deleted.is_empty() {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Deleted units: {}",
                changes.deleted.join(", ")
            );
        }
        if self.sync_only {
            return;
        }
        self.stop_deleted_units(&changes.deleted);
        self.systemd.daemon_reload(self.cfg);

        let start_on_sync = start_on_sync_units(&self.cfg.effective_source_dirs(), self.cfg);
        let plan = self.plan_activation(&changes.changed, &start_on_sync);
        let mut pulled = Self::files_worth_pulling(&changes.changed, &plan);
        // State can change during a long pull, so re-plan after it.
        let plan = if self.pre_pull_images(&pulled) {
            let replanned = self.plan_activation(&changes.changed, &start_on_sync);
            let added: Vec<String> = Self::files_worth_pulling(&changes.changed, &replanned)
                .into_iter()
                .filter(|f| !pulled.contains(f))
                .collect();
            // One extra round only: this pull can be slow too, and chasing
            // the state forever would never reach activation.
            self.pre_pull_images(&added);
            pulled.extend(added);
            replanned
        } else {
            plan
        };

        if self.cfg.verbose {
            let skipped: Vec<&String> = changes
                .changed
                .iter()
                .filter(|f| Self::may_carry_image(f) && !pulled.contains(f))
                .collect();
            if !skipped.is_empty() {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Skipping image pre-pull for units that will not be activated: {}",
                    skipped
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }

        let _failed = self.execute_activation(&plan);
    }

    /// One-shot sync: sync all repos, daemon-reload, then restart changed units.
    pub fn run_once(&self, cd_config: &CDConfig) -> usize {
        let result = self.sync_all(cd_config);
        self.apply_changes(&result.changes);
        result.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;
    use std::cell::RefCell;
    use std::path::Path;

    use super::super::image::testing::MockImagePuller;
    use super::super::systemd::testing::MockSystemd;
    use super::super::vcs::testing::MockVcs;

    // apply_changes: the puller below changes systemd state mid-pull.

    /// Image puller that applies `on_pull` to the systemd mock the first time
    /// it pulls, simulating state changing during a slow download.
    struct StateChangingPuller<'a> {
        systemd: &'a MockSystemd,
        pulled: RefCell<Vec<String>>,
        on_pull: Box<dyn Fn(&MockSystemd) + 'a>,
    }

    impl<'a> StateChangingPuller<'a> {
        fn new(systemd: &'a MockSystemd, on_pull: impl Fn(&MockSystemd) + 'a) -> Self {
            Self {
                systemd,
                pulled: RefCell::new(Vec::new()),
                on_pull: Box::new(on_pull),
            }
        }
    }

    impl ImagePuller for StateChangingPuller<'_> {
        fn pull(&self, image: &ImageRef, _cfg: &Config) {
            if self.pulled.borrow().is_empty() {
                (self.on_pull)(self.systemd);
            }
            self.pulled.borrow_mut().push(image.image.clone());
        }
    }

    /// Write a repo dir with the given files under `data_dir`, so
    /// `effective_source_dirs` picks them up.
    fn write_repo(data_dir: &Path, files: &[(&str, &str)]) {
        let repo_dir = data_dir.join("myrepo");
        fs::create_dir_all(&repo_dir).unwrap();
        for (name, content) in files {
            fs::write(repo_dir.join(name), content).unwrap();
        }
    }

    #[test]
    fn apply_changes_does_not_restart_a_unit_stopped_during_the_pull() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();
        write_repo(
            tmp.path(),
            &[(
                "app.container",
                "[Container]\nImage=quay.io/podman/hello:latest\n",
            )],
        );

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        // The operator stops the unit while the image is downloading.
        let puller = StateChangingPuller::new(&systemd, |systemd| {
            systemd.set_state("app.service", "inactive", "dead");
        });

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &puller);
        runner.apply_changes(&UnitChanges::from_present(
            vec!["app.container".to_string()],
        ));

        assert_eq!(puller.pulled.borrow().len(), 1, "the image is pre-pulled");
        assert!(
            systemd.restarted.borrow().is_empty(),
            "a unit stopped during the pull must not be resurrected"
        );
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn apply_changes_pulls_for_a_unit_the_re_plan_adds() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();
        write_repo(
            tmp.path(),
            &[
                (
                    "app.container",
                    "[Container]\nImage=quay.io/podman/hello:latest\n",
                ),
                (
                    "idle.container",
                    "[Container]\nImage=quay.io/podman/idle:latest\n",
                ),
            ],
        );

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        systemd
            .reverse_deps_map
            .borrow_mut()
            .insert("idle.service".to_string(), vec!["late.target".to_string()]);
        // The target that wants `idle.service` comes up during the pull, so
        // the second plan starts it — with its image pulled after all.
        let puller = StateChangingPuller::new(&systemd, |systemd| {
            systemd.set_active("late.target");
        });

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &puller);
        runner.apply_changes(&UnitChanges::from_present(vec![
            "app.container".to_string(),
            "idle.container".to_string(),
        ]));

        assert_eq!(
            puller.pulled.borrow().as_slice(),
            &[
                "quay.io/podman/hello:latest".to_string(),
                "quay.io/podman/idle:latest".to_string()
            ],
            "the unit the re-plan added must get its image too"
        );
        assert_eq!(systemd.started.borrow().as_slice(), &["idle.service"]);
    }

    #[test]
    fn apply_changes_first_deploy_builds_start_on_sync_image_before_container() {
        // Fresh clone: every file is changed. The build is wanted by nothing,
        // the container uses its image with `Pull=never` and no dependency.
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = tmp.path().to_path_buf();
        write_repo(
            tmp.path(),
            &[
                (
                    "app.build",
                    "[Build]\nImageTag=localhost/app\n[X-QuadCD]\nStartOnSync=true\n",
                ),
                (
                    "app.container",
                    "[Container]\nImage=localhost/app\nPull=never\n[Install]\nWantedBy=default.target\n",
                ),
            ],
        );

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        let puller = MockImagePuller::new();

        let runner = SyncRunner::new(&cfg, &vcs, &systemd, &puller);
        runner.apply_changes(&UnitChanges::from_present(vec![
            "app.build".to_string(),
            "app.container".to_string(),
        ]));

        let actions: Vec<String> = systemd
            .call_log
            .borrow()
            .iter()
            .filter(|c| c.starts_with("start:") || c.starts_with("restart:"))
            .cloned()
            .collect();
        assert_eq!(actions, &["start:app-build.service", "start:app.service"]);
    }
}
