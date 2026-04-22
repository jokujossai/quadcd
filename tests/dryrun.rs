//! Integration tests for the dry-run pipeline.
//!
//! These exercise `DryRunner::run()` end-to-end: preview source files, install
//! into a temp directory, invoke the generator, and print output.

mod common;

use common::{test_config, true_binary, TestWriter};
use quadcd::cd_config::{CDConfig, RepoConfig};
use quadcd::testing::DryRunner;
use quadcd::GeneratorImpl;

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn dryrun_returns_zero() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];

    let runner = DryRunner::new_for_test(&cfg, &args, &gen);

    assert_eq!(runner.run(), 0);
}

#[test]
fn dryrun_previews_source_files() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("local");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("app.container"),
        "Image=quay.io/podman/hello:latest",
    )
    .unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;
    cfg.source_dir = source_dir;
    cfg.quadcd_unit_dirs = Some(cfg.source_dir.to_string_lossy().to_string());

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];

    let runner = DryRunner::new_for_test(&cfg, &args, &gen);

    runner.run();

    let stdout = out_buf.captured();
    assert!(stdout.contains("app.container"));
    assert!(stdout.contains("Image=quay.io/podman/hello:latest"));
}

#[test]
fn dryrun_shows_cd_config_info() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;

    let mut repos = std::collections::HashMap::new();
    repos.insert(
        "myapp".to_string(),
        RepoConfig {
            url: "https://github.com/user/myapp.git".to_string(),
            branch: Some("main".to_string()),
            interval: Some("30s".to_string()),
        },
    );
    cfg.cd_config = Some(CDConfig {
        repositories: repos,
    });

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];

    let runner = DryRunner::new_for_test(&cfg, &args, &gen);

    runner.run();

    let stderr = err_buf.captured();
    assert!(stderr.contains("myapp"));
    assert!(stderr.contains("https://github.com/user/myapp.git"));
    assert!(stderr.contains("main"));
    assert!(stderr.contains("30s"));
}

#[test]
fn dryrun_cd_config_defaults_and_cloned_status() {
    let tmp = tempfile::tempdir().unwrap();
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;
    cfg.data_dir = tmp.path().to_path_buf();

    // Create .git dir for one repo to trigger "cloned" status
    std::fs::create_dir_all(tmp.path().join("cloned-repo/.git")).unwrap();

    let mut repos = std::collections::HashMap::new();
    repos.insert(
        "cloned-repo".to_string(),
        RepoConfig {
            url: "https://example.com/repo.git".to_string(),
            branch: None,   // triggers "(default)"
            interval: None, // triggers "manual"
        },
    );
    cfg.cd_config = Some(CDConfig {
        repositories: repos,
    });

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];
    DryRunner::new_for_test(&cfg, &args, &gen).run();

    let stderr = err_buf.captured();
    assert!(stderr.contains("cloned"));
    assert!(stderr.contains("(default)"));
    assert!(stderr.contains("manual"));
}

#[test]
fn dryrun_previews_systemd_files() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("local");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("app.service"), "ExecStart=/bin/app").unwrap();
    std::fs::write(source_dir.join("app.timer"), "OnCalendar=daily").unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;
    cfg.source_dir = source_dir;
    cfg.quadcd_unit_dirs = Some(cfg.source_dir.to_string_lossy().to_string());

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];
    DryRunner::new_for_test(&cfg, &args, &gen).run();

    let stdout = out_buf.captured();
    assert!(stdout.contains("app.service"));
    assert!(stdout.contains("ExecStart=/bin/app"));
    assert!(stdout.contains("app.timer"));
    assert!(stdout.contains("OnCalendar=daily"));

    let stderr = err_buf.captured();
    assert!(stderr.contains("Systemd units from"));
}

#[test]
fn dryrun_no_source_dirs_verbose() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;
    cfg.quadcd_unit_dirs = None;
    cfg.data_dir = std::path::PathBuf::from("/no/such/dir");

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];
    DryRunner::new_for_test(&cfg, &args, &gen).run();

    let stderr = err_buf.captured();
    assert!(stderr.contains("No source directories found"));
}

#[test]
fn dryrun_nonexistent_source_dir_verbose() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.verbose = true;
    cfg.source_dir = std::path::PathBuf::from("/no/such/source");
    cfg.quadcd_unit_dirs = Some("/no/such/source".to_string());

    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let args = vec!["normal-dir".to_string()];
    DryRunner::new_for_test(&cfg, &args, &gen).run();

    let stderr = err_buf.captured();
    assert!(stderr.contains("does not exist"));
}
