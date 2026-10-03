//! Exclusive sync lock on `data_dir/.quadcd-sync.lock`, so a manual
//! `quadcd sync` and the service never sync at the same time.

use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::Path;

fn open_sync_lock_file(data_dir: &Path) -> Result<fs::File, String> {
    let lock_path = data_dir.join(".quadcd-sync.lock");
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open lock file {}: {e}", lock_path.display()))
}

/// Acquire an exclusive lock on `data_dir/.quadcd-sync.lock`, blocking until
/// any other holder releases it.
///
/// Returns the open `File` handle whose lifetime holds the lock. The lock is
/// released automatically when the handle is dropped.
pub fn acquire_sync_lock(data_dir: &Path) -> Result<fs::File, String> {
    let file = open_sync_lock_file(data_dir)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("Failed to acquire sync lock: {err}"));
    }
    Ok(file)
}

/// Try to acquire an exclusive lock on `data_dir/.quadcd-sync.lock` without
/// blocking.
///
/// Returns `Ok(Some(file))` when the lock was acquired, `Ok(None)` when another
/// process already holds it, or `Err(_)` on a real I/O failure.
pub fn try_acquire_sync_lock(data_dir: &Path) -> Result<Option<fs::File>, String> {
    let file = open_sync_lock_file(data_dir)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(file));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(format!("Failed to acquire sync lock: {err}"))
    }
}
