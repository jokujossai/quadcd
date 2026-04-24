//! Integration tests for `App::run()` — subcommand dispatch, generate flow,
//! sync flow, and systemd generator auto-detection.
//!
//! These exercise the full `App::run()` pipeline with mock dependencies
//! injected via `App::new_with_deps`.

mod common;

use common::{
    make_app, test_config, true_binary, NoopImagePuller, NoopSystemd, NoopVcs, TestWriter,
};
use quadcd::{App, GeneratorImpl};
use std::path::PathBuf;

// ===========================================================================
// Version flag
// ===========================================================================

#[test]
fn version_flag_prints_version() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string(), "-version".to_string()]);

    assert_eq!(code, 0);
    assert!(out_buf.captured().contains("quadcd version"));
}

#[test]
fn version_flag_anywhere_in_args() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&[
        "quadcd".to_string(),
        "generate".to_string(),
        "-version".to_string(),
    ]);

    assert_eq!(code, 0);
    assert!(out_buf.captured().contains("quadcd version"));
}

// ===========================================================================
// Subcommand routing
// ===========================================================================

#[test]
fn unknown_subcommand_returns_error() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string(), "foobar".to_string()]);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("unknown subcommand"));
    assert!(err_buf.captured().contains("foobar"));
}

#[test]
fn run_no_args_shows_usage() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string()]);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("Usage:"));
}

#[test]
fn help_subcommand_shows_usage() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string(), "help".to_string()]);

    assert_eq!(code, 0);
    assert!(err_buf.captured().contains("Usage:"));
}

#[test]
fn version_subcommand_prints_version() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string(), "version".to_string()]);

    assert_eq!(code, 0);
    assert!(out_buf.captured().contains("quadcd version"));
}

// ===========================================================================
// Sync subcommand
// ===========================================================================

#[test]
fn sync_no_config_file_errors() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&["quadcd".to_string(), "sync".to_string()]);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("no config file found"));
}

#[test]
fn sync_invalid_flag_errors() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let code = app.run(&[
        "quadcd".to_string(),
        "sync".to_string(),
        "--unknown".to_string(),
    ]);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("invalid argument"));
}

#[test]
fn sync_empty_repos_errors() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("quadcd.toml"), "[repositories]").unwrap();

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.home = tmp.path().to_string_lossy().to_string();

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let code = app.run(&["quadcd".to_string(), "sync".to_string()]);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("no repositories configured"));
}

#[test]
fn sync_verbose_logs_info() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("quadcd.toml"),
        "[repositories.myrepo]\nurl = \"https://example.com/repo.git\"\n",
    )
    .unwrap();

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.home = tmp.path().to_string_lossy().to_string();

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let code = app.run(&["quadcd".to_string(), "sync".to_string(), "-v".to_string()]);

    assert_eq!(code, 0);
    let stderr = err_buf.captured();
    assert!(stderr.contains("Running in"));
    assert!(stderr.contains("Data dir:"));
    assert!(stderr.contains("1 repository(ies) configured"));
}

#[test]
fn sync_force_and_user_flags() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("quadcd.toml"),
        "[repositories.r]\nurl = \"https://example.com/r.git\"\n",
    )
    .unwrap();

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.home = tmp.path().to_string_lossy().to_string();

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let code = app.run(&[
        "quadcd".to_string(),
        "sync".to_string(),
        "--force".to_string(),
        "--user".to_string(),
    ]);

    assert_eq!(code, 0);
    assert!(app.cfg.force);
    assert!(app.cfg.is_user_mode);
}

#[test]
fn sync_accept_new_host_keys_flag() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("quadcd.toml"),
        "[repositories.r]\nurl = \"https://example.com/r.git\"\n",
    )
    .unwrap();

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.home = tmp.path().to_string_lossy().to_string();

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let code = app.run(&[
        "quadcd".to_string(),
        "sync".to_string(),
        "--accept-new-host-keys".to_string(),
    ]);

    // Should succeed (uses NoopVcs which doesn't actually connect)
    assert_eq!(code, 0);
}

#[test]
fn sync_waits_for_held_lock_then_proceeds() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("quadcd.toml"),
        "[repositories.r]\nurl = \"https://example.com/r.git\"\n",
    )
    .unwrap();

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.home = tmp.path().to_string_lossy().to_string();

    let data_dir = tmp.path().join(".local/share/quadcd");
    std::fs::create_dir_all(&data_dir).unwrap();
    let lock = quadcd::install::acquire_sync_lock(&data_dir).unwrap();

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        drop(lock);
        tx.send(()).unwrap();
    });

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let code = app.run(&["quadcd".to_string(), "sync".to_string()]);

    rx.recv_timeout(Duration::from_secs(5))
        .expect("releaser thread should have dropped the lock");
    assert_eq!(code, 0, "sync should succeed once the lock is released");
}

// ===========================================================================
// Generate subcommand
// ===========================================================================

#[test]
fn generate_missing_normal_dir_errors() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let args = vec!["quadcd".to_string(), "generate".to_string()];
    let code = app.run(&args);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("missing required argument"));
}

#[test]
fn generate_invalid_flag_errors() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "--badarg".to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("invalid argument"));
}

#[test]
fn generate_no_source_dirs_runs_generator() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;

    let fake_gen = true_binary();
    let gen = GeneratorImpl {
        path: fake_gen.clone(),
    };
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some("/no/such/dir".to_string());
    cfg.source_dir = PathBuf::from("/no/such/dir");
    cfg.set_podman_generator_path(Some(fake_gen.to_string_lossy().to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "/tmp/normal".to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
}

#[test]
fn generate_dryrun_implies_verbose() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "-dryrun".to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
    assert!(err_buf.captured().contains("[quadcd]"));
}

#[test]
fn generate_with_sources_installs_and_runs_generator() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("app.container"),
        "Image=quay.io/podman/hello:latest",
    )
    .unwrap();
    std::fs::write(source_dir.join("web.service"), "ExecStart=/bin/web").unwrap();

    let gen_path = tmp.path().join("fake-generator");
    std::fs::write(&gen_path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gen_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let normal_dir = tmp.path().join("normal");
    std::fs::create_dir_all(&normal_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: gen_path.clone(),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some(source_dir.to_string_lossy().to_string());
    cfg.source_dir = source_dir;
    cfg.podman_generator = gen_path.clone();
    cfg.set_podman_generator_path(Some(gen_path.to_string_lossy().to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "-v".to_string(),
        normal_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
    let stderr = err_buf.captured();
    assert!(stderr.contains("Installing from"));
    assert!(stderr.contains("Invoking Podman generator"));
}

#[test]
fn generate_missing_generator_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("app.container"), "").unwrap();

    let normal_dir = tmp.path().join("normal");
    std::fs::create_dir_all(&normal_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: PathBuf::from("/no/such/generator"),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some(source_dir.to_string_lossy().to_string());
    cfg.source_dir = source_dir;
    cfg.podman_generator = PathBuf::from("/no/such/generator");
    cfg.set_podman_generator_path(Some("/no/such/generator".to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        normal_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
    assert!(err_buf
        .captured()
        .contains("Podman generator not found, skipping"));
}

#[test]
fn generate_non_executable_generator_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("app.container"), "").unwrap();

    let gen_path = tmp.path().join("fake-gen");
    std::fs::write(&gen_path, "not executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gen_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let normal_dir = tmp.path().join("normal");
    std::fs::create_dir_all(&normal_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: gen_path.clone(),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some(source_dir.to_string_lossy().to_string());
    cfg.source_dir = source_dir;
    cfg.podman_generator = gen_path.clone();
    cfg.set_podman_generator_path(Some(gen_path.to_string_lossy().to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        normal_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("not found or not executable"));
}

#[test]
fn generate_quadlet_unit_dirs_override() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("app.container"),
        "Image=quay.io/podman/hello:latest",
    )
    .unwrap();

    let gen_path = tmp.path().join("fake-gen");
    std::fs::write(&gen_path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gen_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let normal_dir = tmp.path().join("normal");
    std::fs::create_dir_all(&normal_dir).unwrap();

    let output_dir = tmp.path().join("output");
    std::fs::create_dir_all(&output_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: gen_path.clone(),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some(source_dir.to_string_lossy().to_string());
    cfg.source_dir = source_dir;
    cfg.podman_generator = gen_path.clone();
    cfg.set_podman_generator_path(Some(gen_path.to_string_lossy().to_string()));
    cfg.quadlet_unit_dirs = Some(output_dir.to_string_lossy().to_string());

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "-v".to_string(),
        normal_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
    let stderr = err_buf.captured();
    assert!(stderr.contains("Quadlet dir:"));
    assert!(stderr.contains(&output_dir.to_string_lossy().to_string()));
}

#[test]
fn generate_no_kmsg_log_accepted() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.quadcd_unit_dirs = Some("/no/such/dir".to_string());
    cfg.source_dir = PathBuf::from("/no/such/dir");

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        "generate".to_string(),
        "-no-kmsg-log".to_string(),
        "/tmp/normal".to_string(),
    ];
    let _code = app.run(&args);

    assert!(!err_buf.captured().contains("invalid argument"));
}

#[test]
fn generate_version_flag_inside_run_generate() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let args = vec!["other-generator".to_string(), "-version".to_string()];
    let code = app.run(&args);

    assert_eq!(code, 0);
    assert!(out_buf.captured().contains("quadcd version"));
}

// ===========================================================================
// Systemd generator auto-detection
// ===========================================================================

#[test]
fn systemd_generator_autodetect_enters_generate() {
    let tmp = tempfile::tempdir().unwrap();
    let normal_dir = tmp.path().join("normal");
    std::fs::create_dir_all(&normal_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;

    let fake_gen = true_binary();
    let gen = GeneratorImpl {
        path: fake_gen.clone(),
    };
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.systemd_scope = Some("system".to_string());
    cfg.quadcd_unit_dirs = Some("/no/such/dir".to_string());
    cfg.source_dir = PathBuf::from("/no/such/dir");
    cfg.set_podman_generator_path(Some(fake_gen.to_string_lossy().to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        normal_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
}

#[test]
fn systemd_generator_autodetect_three_args() {
    let tmp = tempfile::tempdir().unwrap();
    let normal_dir = tmp.path().join("normal");
    let early_dir = tmp.path().join("early");
    let late_dir = tmp.path().join("late");
    std::fs::create_dir_all(&normal_dir).unwrap();
    std::fs::create_dir_all(&early_dir).unwrap();
    std::fs::create_dir_all(&late_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;

    let fake_gen = true_binary();
    let gen = GeneratorImpl {
        path: fake_gen.clone(),
    };
    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.systemd_scope = Some("system".to_string());
    cfg.quadcd_unit_dirs = Some("/no/such/dir".to_string());
    cfg.source_dir = PathBuf::from("/no/such/dir");
    cfg.set_podman_generator_path(Some(fake_gen.to_string_lossy().to_string()));

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec![
        "quadcd".to_string(),
        normal_dir.to_string_lossy().to_string(),
        early_dir.to_string_lossy().to_string(),
        late_dir.to_string_lossy().to_string(),
    ];
    let code = app.run(&args);

    assert_eq!(code, 0);
}

#[test]
fn no_autodetect_without_systemd_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let some_dir = tmp.path().join("somedir");
    std::fs::create_dir_all(&some_dir).unwrap();

    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };
    let mut app = make_app(&out_buf, &err_buf, &vcs, &systemd, &gen);

    let args = vec!["quadcd".to_string(), some_dir.to_string_lossy().to_string()];
    let code = app.run(&args);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("unknown subcommand"));
}

#[test]
fn no_autodetect_when_arg_is_not_directory() {
    let out_buf = TestWriter::new();
    let err_buf = TestWriter::new();
    let vcs = NoopVcs;
    let systemd = NoopSystemd;
    let gen = GeneratorImpl {
        path: true_binary(),
    };

    let mut cfg = test_config(&out_buf, &err_buf);
    cfg.systemd_scope = Some("system".to_string());

    let mut app = App::new_with_deps(cfg, &vcs, &systemd, &NoopImagePuller, &gen);
    let args = vec!["quadcd".to_string(), "--some-flag".to_string()];
    let code = app.run(&args);

    assert_eq!(code, 1);
    assert!(err_buf.captured().contains("unknown subcommand"));
}
