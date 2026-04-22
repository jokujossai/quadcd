//! Integration tests for Config: effective_source_dirs with env file merging.

mod common;

use common::{test_config, TestWriter};
use quadcd::config::load_env_file;
use quadcd::output::Output;
use std::fs;

#[test]
fn effective_source_dirs_enumerates_subdirs() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(data_dir.join("alpha")).unwrap();
    fs::create_dir_all(data_dir.join("beta")).unwrap();
    // Base .env shared across all source dirs
    fs::write(data_dir.join(".env"), "X=1\nY=base").unwrap();
    // Per-dir .env for alpha overrides Y, adds Z
    fs::write(data_dir.join("alpha/.env"), "Y=alpha\nZ=extra").unwrap();
    // beta has no per-dir .env

    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.quadcd_unit_dirs = None;
    cfg.data_dir = data_dir.clone();
    let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
    cfg.env_vars = load_env_file(&data_dir, false, &output);

    let dirs = cfg.effective_source_dirs();
    assert_eq!(dirs.len(), 2);
    // Sorted alphabetically
    assert!(dirs[0].0.ends_with("alpha"));
    assert!(dirs[1].0.ends_with("beta"));
    // alpha: X from base, Y overridden, Z added
    assert_eq!(dirs[0].1["X"], "1");
    assert_eq!(dirs[0].1["Y"], "alpha");
    assert_eq!(dirs[0].1["Z"], "extra");
    // beta: inherits base only
    assert_eq!(dirs[1].1["X"], "1");
    assert_eq!(dirs[1].1["Y"], "base");
    assert!(!dirs[1].1.contains_key("Z"));
}

#[test]
fn effective_source_dirs_quadcd_unit_dirs_with_per_dir_env() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("mydir");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join(".env"), "LOCAL=yes\nSHARED=override").unwrap();

    let out = TestWriter::new();
    let err = TestWriter::new();
    let mut cfg = test_config(&out, &err);
    cfg.quadcd_unit_dirs = Some(source_dir.to_string_lossy().to_string());
    cfg.source_dir = source_dir.clone();
    cfg.env_vars
        .insert("SHARED".to_string(), "base".to_string());
    cfg.env_vars
        .insert("BASE_ONLY".to_string(), "yes".to_string());

    let dirs = cfg.effective_source_dirs();
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0].1["LOCAL"], "yes");
    assert_eq!(dirs[0].1["SHARED"], "override");
    assert_eq!(dirs[0].1["BASE_ONLY"], "yes");
}

#[test]
fn source_dirs_verbose_logs_effective_vars() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(data_dir.join("sub")).unwrap();
    fs::write(data_dir.join(".env"), "BASE=1").unwrap();
    fs::write(data_dir.join("sub/.env"), "LOCAL=2").unwrap();

    let out = TestWriter::new();
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&out, &err_buf);
    cfg.verbose = true;
    cfg.quadcd_unit_dirs = None;
    cfg.data_dir = data_dir.clone();
    let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
    cfg.env_vars = load_env_file(&data_dir, false, &output);

    let dirs = cfg.effective_source_dirs();
    assert_eq!(dirs.len(), 1);

    let stderr = err_buf.captured();
    assert!(stderr.contains("Effective variables for sub/"));
}
