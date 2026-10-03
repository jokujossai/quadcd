//! Integration tests for file installation.

mod common;

use common::{test_config, TestWriter};
use quadcd::install::{install_quadlet_files, install_systemd_units};
use std::collections::HashMap;
use std::fs;

// ===========================================================================
// install_quadlet_files
// ===========================================================================

#[test]
fn install_quadlet_files_copies_with_substitution() {
    let src = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let quadlet_dir = out_dir.path().join("quadcd");
    fs::create_dir_all(&quadlet_dir).unwrap();

    let container_src = src.path().join("app.container");
    fs::write(&container_src, "Image=${IMAGE}").unwrap();
    let mut vars = HashMap::new();
    vars.insert(
        "IMAGE".to_string(),
        "quay.io/podman/hello:latest".to_string(),
    );

    let out = TestWriter::new();
    let err = TestWriter::new();
    let cfg = test_config(&out, &err);
    install_quadlet_files(src.path(), &quadlet_dir, &vars, &cfg).unwrap();

    let installed = fs::read_to_string(quadlet_dir.join("app.container")).unwrap();
    assert!(
        installed.contains("Image=quay.io/podman/hello:latest"),
        "content: {installed}"
    );
    let expected_source = format!("SourcePath={}", container_src.display());
    assert!(
        installed.contains(&expected_source),
        "expected {expected_source}, content: {installed}"
    );
}

#[test]
fn install_quadlet_files_substitutes_repo_root() {
    let src = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let quadlet_dir = out_dir.path().join("quadcd");
    fs::create_dir_all(&quadlet_dir).unwrap();

    let container_src = src.path().join("app.container");
    fs::write(
        &container_src,
        "[Container]\nVolume=${QUADCD_REPO_ROOT}/configs/app.yaml:/etc/app.yaml:Z,ro\n",
    )
    .unwrap();

    let mut vars = HashMap::new();
    vars.insert(
        "QUADCD_REPO_ROOT".to_string(),
        src.path().to_string_lossy().into_owned(),
    );

    let out = TestWriter::new();
    let err = TestWriter::new();
    let cfg = test_config(&out, &err);
    install_quadlet_files(src.path(), &quadlet_dir, &vars, &cfg).unwrap();

    let installed = fs::read_to_string(quadlet_dir.join("app.container")).unwrap();
    let expected = format!(
        "Volume={}/configs/app.yaml:/etc/app.yaml:Z,ro",
        src.path().display()
    );
    assert!(
        installed.contains(&expected),
        "expected {expected}, content: {installed}"
    );
}

// ===========================================================================
// install_systemd_units
// ===========================================================================

#[test]
fn install_systemd_units_copies_to_normal_dir() {
    let src = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let normal_dir = out_dir.path().join("normal");

    let service_src = src.path().join("app.service");
    let timer_src = src.path().join("app.timer");
    fs::write(&service_src, "ExecStart=${CMD}").unwrap();
    fs::write(&timer_src, "OnCalendar=daily").unwrap();

    let mut vars = HashMap::new();
    vars.insert("CMD".to_string(), "/usr/bin/app".to_string());
    let out = TestWriter::new();
    let err = TestWriter::new();
    let cfg = test_config(&out, &err);

    install_systemd_units(src.path(), &normal_dir, &vars, &cfg).unwrap();

    let service = fs::read_to_string(normal_dir.join("app.service")).unwrap();
    assert!(
        service.contains("ExecStart=/usr/bin/app"),
        "content: {service}"
    );
    let expected_source = format!("SourcePath={}", service_src.display());
    assert!(
        service.contains(&expected_source),
        "expected {expected_source}, content: {service}"
    );

    let timer = fs::read_to_string(normal_dir.join("app.timer")).unwrap();
    assert!(timer.contains("OnCalendar=daily"), "content: {timer}");
    let expected_source = format!("SourcePath={}", timer_src.display());
    assert!(
        timer.contains(&expected_source),
        "expected {expected_source}, content: {timer}"
    );
}
