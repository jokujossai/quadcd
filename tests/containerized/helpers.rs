//! Shared helpers for containerized integration tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Mode detection
// ---------------------------------------------------------------------------

pub fn is_user_mode() -> bool {
    unsafe { libc::getuid() != 0 }
}

// ---------------------------------------------------------------------------
// Mode-aware paths
// ---------------------------------------------------------------------------

pub fn config_path() -> &'static str {
    if is_user_mode() {
        "/home/quadcd-test/.config/quadcd.toml"
    } else {
        "/etc/quadcd.toml"
    }
}

pub fn data_dir() -> &'static str {
    if is_user_mode() {
        "/home/quadcd-test/.local/share/quadcd"
    } else {
        "/var/lib/quadcd"
    }
}

pub fn repos_dir() -> String {
    let uid = unsafe { libc::getuid() };
    format!("/tmp/quadcd-test-repos-{uid}")
}

pub const SERVICE_NAME: &str = "quadcd-sync.service";

// ---------------------------------------------------------------------------
// Helpers: git
// ---------------------------------------------------------------------------

pub fn run_git(args: &[&str]) {
    let status = Command::new("git").args(args).status().unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

pub fn run_git_in(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} in {:?} failed", args, dir);
}

/// Create a bare git repo with an initial commit containing the given files.
pub fn create_bare_repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repos = PathBuf::from(repos_dir());
    fs::create_dir_all(&repos).unwrap();

    let bare = repos.join(format!("{name}.git"));
    let work = repos.join(format!("{name}-work"));

    // Clean up any prior runs
    if bare.exists() {
        fs::remove_dir_all(&bare).unwrap();
    }
    if work.exists() {
        fs::remove_dir_all(&work).unwrap();
    }

    run_git(&["init", "--bare", "-b", "main", bare.to_str().unwrap()]);
    run_git(&["clone", bare.to_str().unwrap(), work.to_str().unwrap()]);
    run_git_in(&work, &["config", "user.email", "test@test.com"]);
    run_git_in(&work, &["config", "user.name", "Test"]);

    for (filename, content) in files {
        fs::write(work.join(filename), content).unwrap();
    }
    run_git_in(&work, &["add", "."]);
    run_git_in(&work, &["commit", "-m", "initial"]);
    run_git_in(&work, &["push", "-u", "origin", "main"]);

    fs::remove_dir_all(&work).unwrap();
    bare
}

/// Push an additional commit to a bare repo via a temporary clone.
pub fn push_commit(bare: &Path, files: &[(&str, &str)], message: &str) {
    let work = bare.with_extension("push-tmp");
    if work.exists() {
        fs::remove_dir_all(&work).unwrap();
    }
    run_git(&["clone", bare.to_str().unwrap(), work.to_str().unwrap()]);
    run_git_in(&work, &["config", "user.email", "test@test.com"]);
    run_git_in(&work, &["config", "user.name", "Test"]);
    for (filename, content) in files {
        fs::write(work.join(filename), content).unwrap();
    }
    run_git_in(&work, &["add", "."]);
    run_git_in(&work, &["commit", "-m", message]);
    run_git_in(&work, &["push", "origin", "main"]);
    fs::remove_dir_all(&work).unwrap();
}

/// Create a bare git repo on a non-default branch with an initial commit.
pub fn create_bare_repo_on_branch(name: &str, branch: &str, files: &[(&str, &str)]) -> PathBuf {
    let repos = PathBuf::from(repos_dir());
    fs::create_dir_all(&repos).unwrap();

    let bare = repos.join(format!("{name}.git"));
    let work = repos.join(format!("{name}-work"));

    if bare.exists() {
        fs::remove_dir_all(&bare).unwrap();
    }
    if work.exists() {
        fs::remove_dir_all(&work).unwrap();
    }

    run_git(&["init", "--bare", "-b", branch, bare.to_str().unwrap()]);
    run_git(&["clone", bare.to_str().unwrap(), work.to_str().unwrap()]);
    run_git_in(&work, &["config", "user.email", "test@test.com"]);
    run_git_in(&work, &["config", "user.name", "Test"]);

    for (filename, content) in files {
        fs::write(work.join(filename), content).unwrap();
    }
    run_git_in(&work, &["add", "."]);
    run_git_in(&work, &["commit", "-m", "initial"]);
    run_git_in(&work, &["push", "-u", "origin", branch]);

    fs::remove_dir_all(&work).unwrap();
    bare
}

pub fn head_sha(repo_dir: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// Helpers: systemd service
// ---------------------------------------------------------------------------

pub fn systemctl(args: &[&str]) -> bool {
    let mut cmd = Command::new("systemctl");
    if is_user_mode() {
        cmd.arg("--user");
    }
    cmd.args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn start_sync_service() {
    assert!(
        systemctl(&["start", SERVICE_NAME]),
        "failed to start {SERVICE_NAME}"
    );
}

pub fn stop_sync_service() {
    let _ = systemctl(&["stop", SERVICE_NAME]);
}

pub fn is_service_active() -> bool {
    systemctl(&["is-active", "--quiet", SERVICE_NAME])
}

// ---------------------------------------------------------------------------
// Helpers: polling
// ---------------------------------------------------------------------------

/// Poll until `condition` returns true, or panic after `timeout`.
pub fn wait_until(timeout: Duration, description: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(500));
    }
    panic!("Timed out waiting for: {description}");
}

pub fn wait_for_file(repo_name: &str, filename: &str, timeout: Duration) {
    let target = PathBuf::from(data_dir()).join(repo_name).join(filename);
    let desc = format!("{} to appear", target.display());
    wait_until(timeout, &desc, || target.exists());
}

/// Check whether a systemd unit has been started (even if it already exited).
///
/// The test units use `Type=oneshot` with `RemainAfterExit=yes` so that
/// `ActiveEnterTimestamp` is reliably set even after ExecStart exits.
pub fn was_unit_started(unit: &str) -> bool {
    let mut cmd = Command::new("systemctl");
    if is_user_mode() {
        cmd.arg("--user");
    }
    let output = cmd
        .args(["show", "-p", "ActiveEnterTimestamp", unit])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If started, line is "ActiveEnterTimestamp=<timestamp>"; if never started, value is empty
    !stdout.trim().ends_with('=')
}

pub fn wait_for_unit_start(unit: &str, timeout: Duration) {
    let desc = format!("{unit} to have been started");
    wait_until(timeout, &desc, || was_unit_started(unit));
}

pub fn service_main_pid(unit: &str) -> Option<u32> {
    let mut cmd = Command::new("systemctl");
    if is_user_mode() {
        cmd.arg("--user");
    }
    let output = cmd.args(["show", "-p", "MainPID", unit]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "MainPID=12345"
    stdout
        .trim()
        .strip_prefix("MainPID=")
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&pid| pid != 0)
}

/// Check whether journalctl output for the sync service contains the given
/// substring. Searches logs since `since` (a systemd timestamp like "30s ago").
pub fn journal_contains(since: &str, needle: &str) -> bool {
    let unit_flag = if is_user_mode() { "--user-unit" } else { "-u" };
    let output = Command::new("journalctl")
        .args([
            unit_flag,
            SERVICE_NAME,
            "--since",
            since,
            "--no-pager",
            "-o",
            "cat",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(needle)
}

// ---------------------------------------------------------------------------
// Helpers: quadcd binary
// ---------------------------------------------------------------------------

pub const QUADCD_BIN: &str = "/usr/local/bin/quadcd";

pub fn run_quadcd(args: &[&str]) -> std::process::Output {
    Command::new(QUADCD_BIN)
        .args(args)
        .output()
        .expect("failed to execute quadcd")
}

// ---------------------------------------------------------------------------
// Fixture: cleanup between tests
// ---------------------------------------------------------------------------

pub struct SyncTestContext;

impl SyncTestContext {
    pub fn new() -> Self {
        // Stop any running service from a prior test
        stop_sync_service();

        let data = Path::new(data_dir());
        let repos = PathBuf::from(repos_dir());

        // Clean data dir
        if data.exists() {
            let _ = fs::remove_dir_all(data);
        }
        fs::create_dir_all(data).unwrap();

        // Clean repos dir
        if repos.exists() {
            let _ = fs::remove_dir_all(&repos);
        }

        // Ensure config parent dir exists (for user mode: ~/.config/)
        let cfg = Path::new(config_path());
        if let Some(parent) = cfg.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        // Remove stale config
        let _ = fs::remove_file(config_path());

        SyncTestContext
    }
}

impl Drop for SyncTestContext {
    fn drop(&mut self) {
        stop_sync_service();
        // Remove all cached images so tests start with a clean slate.
        let _ = Command::new("podman")
            .args(["image", "prune", "-af"])
            .status();
        let _ = fs::remove_dir_all(data_dir());
        let _ = fs::remove_dir_all(repos_dir());
        let _ = fs::remove_file(config_path());
        // Daemon-reload clears generator output so units from prior tests
        // don't leak into the next test.
        let _ = systemctl(&["daemon-reload"]);
    }
}
