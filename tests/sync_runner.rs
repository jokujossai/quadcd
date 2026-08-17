//! Integration tests for `SyncRunner::run_once` and `SyncRunner::sync_all`.

mod common;

use common::{test_config, TestWriter};
use quadcd::cd_config::{CDConfig, RepoConfig};
use quadcd::sync::{SyncRunner, UnitChanges};
use quadcd::testing::{MockImagePuller, MockSystemd, MockVcs};
use std::collections::HashMap;
use std::fs;

/// A `CDConfig` with the single `myrepo` repository the tests below sync.
fn single_repo_config() -> CDConfig {
    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    CDConfig {
        repositories: repos,
    }
}

// ===========================================================================
// SyncRunner::run_once
// ===========================================================================

#[test]
fn run_once_syncs_and_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    let systemd = MockSystemd::new();
    systemd.reverse_deps_map.borrow_mut().insert(
        "app.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd.set_active("default.target");
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(repo_dir.join("app.container"), "").unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let failures = runner.run_once(&cd_config);

    assert_eq!(failures, 0);
    assert!(!vcs.clone_called.borrow().is_empty());
    assert!(*systemd.reload_called.borrow());
    assert!(systemd
        .started
        .borrow()
        .contains(&"app.service".to_string()));
}

#[test]
fn run_once_no_changes_no_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("same".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("same".to_string());
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let failures = runner.run_once(&cd_config);

    assert_eq!(failures, 0);
    assert!(!*systemd.reload_called.borrow());
    assert!(systemd.restarted.borrow().is_empty());
}

#[test]
fn run_once_pre_pulls_changed_container_images() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    let systemd = MockSystemd::new();
    systemd.reverse_deps_map.borrow_mut().insert(
        "app.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd.set_active("default.target");
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("app.container"),
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.run_once(&cd_config);

    let pulled = image_puller.pulled.borrow();
    assert_eq!(pulled.as_slice(), &["quay.io/podman/hello:latest"]);
}

#[test]
fn run_once_skips_pre_pull_for_units_that_stay_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();
    cfg.verbose = true;

    let vcs = MockVcs::new();
    // `app.service` is active, so it is restarted and its image is pulled.
    // `idle.service` is inactive and wanted only by an inactive unit, so it
    // stays stopped — pulling its image would waste network and disk.
    let systemd = MockSystemd::new();
    systemd.set_active("app.service");
    systemd.reverse_deps_map.borrow_mut().insert(
        "idle.service".to_string(),
        vec!["stopped.target".to_string()],
    );
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("app.container"),
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();
    fs::write(
        repo_dir.join("idle.container"),
        "[Container]\nImage=quay.io/podman/idle:latest\n",
    )
    .unwrap();
    // A stopped unit with no image at all: it must not show up in the
    // pre-pull skip message, which is about images.
    fs::write(repo_dir.join("backup.timer"), "[Timer]\nOnCalendar=daily\n").unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.run_once(&cd_config);

    let pulled = image_puller.pulled.borrow();
    assert_eq!(
        pulled.as_slice(),
        &["quay.io/podman/hello:latest"],
        "only the restarted unit's image should be pulled"
    );
    assert!(
        systemd.started.borrow().is_empty(),
        "the inactive unit should not be started"
    );

    let stderr = err.captured();
    assert!(
        stderr.contains(
            "Skipping image pre-pull for units that will not be activated: idle.container\n"
        ),
        "skipped pre-pull should be logged in verbose mode, got: {stderr}"
    );
    let pre_pull_line = stderr
        .lines()
        .find(|l| l.contains("Skipping image pre-pull"))
        .unwrap_or_default();
    assert!(
        !pre_pull_line.contains("backup.timer"),
        "a unit file that never had an image should not be reported as a skipped pre-pull: {pre_pull_line}"
    );
    // A pull happened, so the plan was recomputed afterwards — the decisions
    // must still be reported exactly once.
    assert_eq!(
        stderr.matches("Skipping inactive idle.service").count(),
        1,
        "re-planning after the pull must not duplicate log lines: {stderr}"
    );
}

#[test]
fn run_once_pre_pulls_image_unit_required_by_a_starting_container() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    // `web.service` is inactive but wanted by an active `default.target`, so
    // sync starts it. `web-image.service` is only required by `web.service`;
    // systemd starts it as part of that transaction, so its image still has
    // to be pre-pulled.
    let systemd = MockSystemd::new();
    systemd.set_active("default.target");
    systemd.reverse_deps_map.borrow_mut().insert(
        "web.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd.reverse_deps_map.borrow_mut().insert(
        "web-image.service".to_string(),
        vec!["web.service".to_string()],
    );
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("web.container"),
        "[Container]\nImage=web.image\n",
    )
    .unwrap();
    fs::write(
        repo_dir.join("web.image"),
        "[Image]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.run_once(&cd_config);

    assert_eq!(
        image_puller.pulled.borrow().as_slice(),
        &["quay.io/podman/hello:latest"],
        "the image unit's image should be pre-pulled for the starting container"
    );
    assert_eq!(
        systemd.started.borrow().as_slice(),
        &["web.service"],
        "systemd pulls in the image unit; sync should not start it itself"
    );
}

#[test]
fn run_once_pulls_nothing_when_no_unit_is_activated() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    // Nothing is active and nothing wants the unit: no start, no restart.
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("app.container"),
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.run_once(&cd_config);

    assert!(
        *systemd.reload_called.borrow(),
        "daemon-reload still runs so systemd sees the new file"
    );
    assert!(
        image_puller.pulled.borrow().is_empty(),
        "no image should be pulled when nothing is activated"
    );
    assert!(systemd.started.borrow().is_empty());
    assert!(systemd.restarted.borrow().is_empty());
}

#[test]
fn run_once_returns_failure_count() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.remote_url_val.borrow_mut() = Err("network error".to_string());
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let failures = runner.run_once(&cd_config);
    assert_eq!(failures, 1);
    assert!(!*systemd.reload_called.borrow());
}

// ===========================================================================
// SyncRunner::sync_only
// ===========================================================================

#[test]
fn run_once_sync_only_skips_reload_and_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    let systemd = MockSystemd::new();
    systemd.reverse_deps_map.borrow_mut().insert(
        "app.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd.set_active("default.target");
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(repo_dir.join("app.container"), "").unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller).sync_only(true);

    let cd_config = single_repo_config();

    let failures = runner.run_once(&cd_config);

    assert_eq!(failures, 0);
    assert!(
        !vcs.clone_called.borrow().is_empty(),
        "repo should be synced"
    );
    assert!(
        !*systemd.reload_called.borrow(),
        "daemon-reload should be skipped"
    );
    assert!(
        systemd.started.borrow().is_empty(),
        "no units should be started"
    );
    assert!(
        image_puller.pulled.borrow().is_empty(),
        "no images should be pulled"
    );

    let stderr = err.captured();
    assert!(
        stderr.contains("Changed units:"),
        "changed units should be listed"
    );
}

// ===========================================================================
// SyncRunner::sync_all
// ===========================================================================

#[test]
fn sync_all_error_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out, &err_buf);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.remote_url_val.borrow_mut() = Err("network error".to_string());
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let result = runner.sync_all(&cd_config);
    assert!(result.changes.is_empty());
    assert_eq!(result.failures, 1);

    let stderr = err_buf.captured();
    assert!(stderr.contains("Error syncing"));
    assert!(stderr.contains("network error"));
}

#[test]
fn sync_all_updated_empty_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out, &err_buf);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
    *vcs.changed_files_val.borrow_mut() = UnitChanges::default();
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.sync_all(&cd_config);

    let stderr = err_buf.captured();
    assert!(stderr.contains("no unit files changed"));
}

#[test]
fn sync_all_up_to_date_verbose() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out, &err_buf);
    cfg.data_dir = tmp.path().to_path_buf();
    cfg.verbose = true;

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("same".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("same".to_string());
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    runner.sync_all(&cd_config);

    let stderr = err_buf.captured();
    assert!(stderr.contains("already up to date"));
}

#[test]
fn sync_all_updated_with_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out, &err_buf);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
    *vcs.changed_files_val.borrow_mut() =
        UnitChanges::from_present(vec!["app.container".to_string()]);
    let systemd = MockSystemd::new();
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let result = runner.sync_all(&cd_config);
    assert_eq!(result.changes.changed, vec!["app.container"]);
    assert!(result.changes.deleted.is_empty());
    assert_eq!(result.failures, 0);

    let stderr = err_buf.captured();
    assert!(stderr.contains("1 unit(s) changed, 0 deleted"));
    assert!(stderr.contains("Sync summary: 1 updated repository"));
}

#[test]
fn run_once_stops_deleted_units_before_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
    *vcs.changed_files_val.borrow_mut() = UnitChanges {
        changed: vec!["web.container".to_string()],
        deleted: vec!["gone.container".to_string()],
    };
    let systemd = MockSystemd::new();
    systemd.set_active("gone.service");
    systemd.reverse_deps_map.borrow_mut().insert(
        "web.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd.set_active("default.target");
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let cd_config = single_repo_config();

    let failures = runner.run_once(&cd_config);

    assert_eq!(failures, 0);
    assert!(systemd
        .stopped
        .borrow()
        .contains(&"gone.service".to_string()));

    // Ordering: stop must precede daemon-reload, and reload must precede start.
    let log = systemd.call_log.borrow();
    let stop_idx = log.iter().position(|s| s == "stop:gone.service").unwrap();
    let reload_idx = log.iter().position(|s| s == "reload").unwrap();
    let start_idx = log.iter().position(|s| s == "start:web.service").unwrap();
    assert!(
        stop_idx < reload_idx,
        "stop must run before daemon-reload; log: {log:?}"
    );
    assert!(
        reload_idx < start_idx,
        "daemon-reload must run before start; log: {log:?}"
    );

    let stderr = err.captured();
    assert!(stderr.contains("Deleted units: gone.container"));
}

#[test]
fn run_once_sync_only_does_not_stop_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.data_dir = tmp.path().to_path_buf();

    let vcs = MockVcs::new();
    *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
    *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
    *vcs.changed_files_val.borrow_mut() = UnitChanges {
        changed: vec![],
        deleted: vec!["gone.container".to_string()],
    };
    let systemd = MockSystemd::new();
    systemd.set_active("gone.service");
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller).sync_only(true);

    let cd_config = single_repo_config();

    runner.run_once(&cd_config);

    assert!(systemd.stopped.borrow().is_empty());
    assert!(!*systemd.reload_called.borrow());
}
