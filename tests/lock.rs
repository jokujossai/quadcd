//! Integration tests for the sync lock.

use quadcd::lock::{acquire_sync_lock, try_acquire_sync_lock};

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
