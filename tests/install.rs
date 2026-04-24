//! Integration tests for file installation and sync locking.

mod common;

use common::{test_config, TestWriter};
use quadcd::install::{
    acquire_sync_lock, install_quadlet_files, install_systemd_units, try_acquire_sync_lock,
};
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

// ===========================================================================
// acquire_sync_lock
// ===========================================================================

#[test]
fn acquire_sync_lock_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let lock = acquire_sync_lock(tmp.path());
    assert!(lock.is_ok());
    assert!(tmp.path().join(".quadcd-sync.lock").exists());
}

#[test]
fn acquire_sync_lock_released_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let _lock = acquire_sync_lock(tmp.path()).unwrap();
    }
    assert!(acquire_sync_lock(tmp.path()).is_ok());
}

#[test]
fn try_acquire_sync_lock_returns_none_when_held() {
    let tmp = tempfile::tempdir().unwrap();
    let _held = acquire_sync_lock(tmp.path()).unwrap();
    let result = try_acquire_sync_lock(tmp.path()).unwrap();
    assert!(
        result.is_none(),
        "try_acquire_sync_lock should report contention as Ok(None)"
    );
}

#[test]
fn try_acquire_sync_lock_succeeds_when_free() {
    let tmp = tempfile::tempdir().unwrap();
    let result = try_acquire_sync_lock(tmp.path()).unwrap();
    assert!(result.is_some());
}

#[test]
fn acquire_sync_lock_blocks_until_released() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let held = acquire_sync_lock(tmp.path()).unwrap();

    let path = tmp.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let lock = acquire_sync_lock(&path).unwrap();
        tx.send(()).unwrap();
        drop(lock);
    });

    // The waiting thread must not acquire the lock while `held` is alive.
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "blocking acquire should wait for the holder"
    );

    drop(held);
    rx.recv_timeout(Duration::from_secs(2))
        .expect("blocking acquire should proceed once the holder drops");
    handle.join().unwrap();
}
