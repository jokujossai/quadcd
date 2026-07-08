//! Integration tests for `SyncRunner::run_once` and `SyncRunner::sync_all`.

mod common;

use common::{test_config, TestWriter};
use quadcd::cd_config::{CDConfig, RepoConfig};
use quadcd::sync::{SyncRunner, UnitChanges};
use quadcd::testing::{MockImagePuller, MockSystemd, MockVcs};
use std::collections::HashMap;
use std::fs;

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
    systemd
        .active_set
        .borrow_mut()
        .insert("default.target".to_string());
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(repo_dir.join("app.container"), "").unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("app.container"),
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

    runner.run_once(&cd_config);

    let pulled = image_puller.pulled.borrow();
    assert_eq!(pulled.as_slice(), &["quay.io/podman/hello:latest"]);
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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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
    systemd
        .active_set
        .borrow_mut()
        .insert("default.target".to_string());
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(repo_dir.join("app.container"), "").unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller).sync_only(true);

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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
    systemd
        .active_set
        .borrow_mut()
        .insert("gone.service".to_string());
    systemd.reverse_deps_map.borrow_mut().insert(
        "web.service".to_string(),
        vec!["default.target".to_string()],
    );
    systemd
        .active_set
        .borrow_mut()
        .insert("default.target".to_string());
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller);

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

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
    systemd
        .active_set
        .borrow_mut()
        .insert("gone.service".to_string());
    let image_puller = MockImagePuller::new();

    let repo_dir = tmp.path().join("myrepo");
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let runner = SyncRunner::new(&cfg, &vcs, &systemd, &image_puller).sync_only(true);

    let mut repos = HashMap::new();
    repos.insert(
        "myrepo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,
            interval: None,
        },
    );
    let cd_config = CDConfig {
        repositories: repos,
    };

    runner.run_once(&cd_config);

    assert!(systemd.stopped.borrow().is_empty());
    assert!(!*systemd.reload_called.borrow());
}
