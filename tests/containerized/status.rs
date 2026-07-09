//! Containerized integration tests for `quadcd status`.
//!
//! These exercise the real CLI against real git, real systemd, and the real
//! quadcd binary at `/usr/local/bin/quadcd`. All tests are `#[ignore]` so
//! `cargo test` on a dev machine skips them; run with `--ignored` inside the
//! test container.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::helpers::*;

// ---------------------------------------------------------------------------
// Local helpers (status-specific)
// ---------------------------------------------------------------------------

/// Run `quadcd status [args...]` with `--user` automatically appended when
/// running as non-root, mirroring how the sync tests invoke quadcd.
fn run_status(extra_args: &[&str]) -> std::process::Output {
    let mut args: Vec<&str> = vec!["status"];
    args.extend_from_slice(extra_args);
    if is_user_mode() {
        args.push("--user");
    }
    run_quadcd(&args)
}

fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Write a single-repo config with a long polling interval so the sync
/// service won't auto-pull a pushed commit during the test window.
fn write_single_repo_config(repo_name: &str, url: &str) {
    fs::write(
        config_path(),
        format!(
            "[repositories.{repo_name}]\nurl = \"{url}\"\nbranch = \"main\"\ninterval = \"60s\"\n"
        ),
    )
    .unwrap();
}

/// Create a bare repo, write a config pointing at it, start the sync
/// service, and wait for the first file from `files` to appear in the data
/// dir. Returns the bare-repo path.
///
/// This goes through the real sync service (not `quadcd sync --sync-only`)
/// so units are installed by the generator, registered with systemd via
/// `daemon-reload`, and started — which is what `status` is meant to
/// observe.
fn prime_repo_via_service(repo_name: &str, files: &[(&str, &str)]) -> PathBuf {
    let bare = create_bare_repo(repo_name, files);
    write_single_repo_config(repo_name, bare.to_str().unwrap());
    start_sync_service();
    if let Some((first, _)) = files.first() {
        wait_for_file(repo_name, first, Duration::from_secs(15));
    }
    bare
}

/// Parse `quadcd status --json` stdout into a `serde_json::Value`, panicking
/// with both stdout and stderr on parse failure so debugging is easy.
fn parse_status_json(out: &std::process::Output) -> serde_json::Value {
    let stdout = stdout_str(out);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "status --json did not produce valid JSON: {e}\nstdout:\n{stdout}\nstderr:\n{}",
            stderr_str(out)
        )
    })
}

/// Read `FragmentPath` for a unit via `systemctl show`.
fn fragment_path(unit: &str) -> PathBuf {
    let mut cmd = Command::new("systemctl");
    if is_user_mode() {
        cmd.arg("--user");
    }
    let out = cmd
        .args(["show", "-p", "FragmentPath", unit])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix("FragmentPath=")
        .map(PathBuf::from)
        .expect("FragmentPath property missing")
}

/// Bump `path`'s mtime past systemd's cached load time. Falls back to a
/// rewrite when `touch -d "+1 minute"` is unavailable.
fn bump_mtime(path: &std::path::Path) {
    let touched = Command::new("touch")
        .args(["-m", "-d", "+1 minute", path.to_str().unwrap()])
        .status()
        .ok()
        .is_some_and(|s| s.success());
    if !touched {
        let content = fs::read_to_string(path).unwrap();
        fs::write(path, content).unwrap();
    }
}

// ---------------------------------------------------------------------------
// CLI plumbing (no sync needed)
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn status_no_config_exits_one() {
    let _ctx = SyncTestContext::new();

    let out = run_status(&["--no-fetch"]);
    assert!(!out.status.success(), "expected non-zero exit");
    assert!(
        stderr_str(&out).contains("no config file found"),
        "stderr: {}",
        stderr_str(&out)
    );
}

#[test]
#[ignore]
fn status_help_returns_zero() {
    let _ctx = SyncTestContext::new();

    let out = run_status(&["--help"]);
    assert!(out.status.success(), "expected zero exit");
    assert!(
        stderr_str(&out).contains("quadcd status"),
        "stderr: {}",
        stderr_str(&out)
    );
}

#[test]
#[ignore]
fn status_missing_repo_reports_missing_and_exits_one() {
    let _ctx = SyncTestContext::new();

    // Config references a bare repo URL but the sync service is never
    // started, so the data dir is empty.
    let bare = create_bare_repo(
        "ghost",
        &[(
            "noop.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("ghost", bare.to_str().unwrap());

    let out = run_status(&["--no-fetch"]);
    let combined = format!("{}\n{}", stdout_str(&out), stderr_str(&out));
    assert!(!out.status.success(), "expected exit 1, got: {combined}");
    assert!(combined.contains("missing"), "stdout: {combined}");
}

// ---------------------------------------------------------------------------
// Repo state (real sync service)
// ---------------------------------------------------------------------------

/// Happy-path test: clean state after a successful sync covers four
/// behaviours that all need the same primed repo, so they are bundled into
/// one test to keep the integration suite fast:
///   1. exit code 0 when nothing is wrong,
///   2. plain-text output has the expected column headers and values,
///   3. `--json` output is parseable and has all required fields,
///   4. the managed service is reported as active with no pending reload.
#[test]
#[ignore]
fn status_clean_state_after_sync() {
    let _ctx = SyncTestContext::new();

    prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    // (1) + (2): plain-text output and exit code.
    let plain = run_status(&["--no-fetch"]);
    let plain_stdout = stdout_str(&plain);
    assert!(
        plain.status.success(),
        "expected exit 0, got stdout:\n{plain_stdout}\nstderr:\n{}",
        stderr_str(&plain)
    );
    for needle in [
        "Repositories",
        "NAME",
        "BRANCH",
        "STATE",
        "HEAD",
        "Services",
        "UNIT",
        "ACTIVE",
        "RELOAD",
        "RESTART",
        "UPTIME",
        "NOTE",
        "up-to-date",
        "myapp",
        "hello.service",
    ] {
        assert!(
            plain_stdout.contains(needle),
            "expected '{needle}' in plain output, got:\n{plain_stdout}"
        );
    }

    // (3) + (4): JSON shape and service-state fields.
    let json = run_status(&["--no-fetch", "--json"]);
    assert!(
        json.status.success(),
        "expected exit 0 for --json, stderr: {}",
        stderr_str(&json)
    );
    let v = parse_status_json(&json);
    assert!(v["mode"].is_string());
    assert!(v["repos"].is_array());
    assert!(v["services"].is_array());

    let repos = v["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"], "myapp");
    assert_eq!(repos[0]["branch"], "main");
    assert_eq!(repos[0]["state"]["state"], "up-to-date");
    assert!(repos[0]["head_sha"].is_string());

    let services = v["services"].as_array().unwrap();
    let svc = services
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .unwrap_or_else(|| panic!("expected hello.service in {services:?}"))
        .clone();
    assert_eq!(svc["active_state"], "active");
    assert_eq!(svc["result"], "success");
    assert_eq!(svc["needs_daemon_reload"], false);
    assert!(svc["enabled"].is_string());
    assert!(svc["restart_pending"].is_boolean());
    assert!(svc["restart_loop_suspected"].is_boolean());
    assert!(svc["n_restarts"].is_number());
}

/// Default-fetch detects upstream commits and reports `behind N` (plain and
/// JSON). Pushes two commits so the count itself is exercised, not just the
/// "non-zero" branch.
#[test]
#[ignore]
fn status_behind_after_upstream_commits() {
    let _ctx = SyncTestContext::new();

    let bare = prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    // Stop the sync service so it can't pull the new upstream commits before
    // we observe them via `quadcd status`.
    stop_sync_service();
    push_commit(&bare, &[("a.txt", "1")], "one");
    push_commit(&bare, &[("b.txt", "2")], "two");

    // Plain output reports the count.
    let plain = run_status(&[]);
    let plain_stdout = stdout_str(&plain);
    assert!(
        !plain.status.success(),
        "expected exit 1, stdout: {plain_stdout}"
    );
    assert!(plain_stdout.contains("behind 2"), "stdout: {plain_stdout}");

    // JSON encodes the tagged variant with `state=behind, commits=2`.
    let json = run_status(&["--json"]);
    assert!(!json.status.success());
    let v = parse_status_json(&json);
    let state = &v["repos"][0]["state"];
    assert_eq!(state["state"], "behind");
    assert_eq!(state["commits"], 2);
}

#[test]
#[ignore]
fn status_no_fetch_does_not_detect_upstream_change() {
    let _ctx = SyncTestContext::new();

    let bare = prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    stop_sync_service();
    push_commit(&bare, &[("extra.txt", "x\n")], "extra commit");

    // --no-fetch must not see the new upstream commit (no network call).
    let out = run_status(&["--no-fetch"]);
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("up-to-date"),
        "expected up-to-date (no fetch), got: {stdout}"
    );
    assert!(
        out.status.success(),
        "expected exit 0, got stdout: {stdout}"
    );
}

#[test]
#[ignore]
fn status_url_mismatch_detected_and_does_not_fetch() {
    let _ctx = SyncTestContext::new();

    prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    // Stop the sync service before rewriting the config, otherwise it will
    // try to sync the bogus URL and may log spurious errors.
    stop_sync_service();
    write_single_repo_config(
        "myapp",
        "/nonexistent/path/that/should/never/be/touched.git",
    );

    let out = run_status(&[]);
    let stdout = stdout_str(&out);
    assert!(!out.status.success(), "expected exit 1, got: {stdout}");
    assert!(
        stdout.contains("url mismatch"),
        "expected 'url mismatch' marker, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Service state — daemon-reload pending, restart pending, restart loop
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn status_detects_daemon_reload_pending_when_unit_file_modified() {
    let _ctx = SyncTestContext::new();

    prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    // Stop the sync service so it doesn't notice the change and run its own
    // daemon-reload before our assertion.
    stop_sync_service();

    let fragment = fragment_path("hello.service");
    assert!(
        fragment.exists(),
        "expected fragment file to exist at {fragment:?}"
    );
    bump_mtime(&fragment);

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .expect("hello.service should appear")
        .clone();
    assert_eq!(
        svc["needs_daemon_reload"], true,
        "expected NeedDaemonReload=yes after bumping fragment mtime, svc: {svc}"
    );
    assert!(!out.status.success(), "expected exit 1 when reload pending");
}

#[test]
#[ignore]
fn status_detects_restart_pending_after_reload_without_restart() {
    let _ctx = SyncTestContext::new();

    prime_repo_via_service("myapp", &[("hello.service", oneshot_unit().as_str())]);
    wait_for_unit_start("hello.service", Duration::from_secs(15));

    // Stop the sync service so it doesn't restart the unit during our setup.
    stop_sync_service();

    let fragment = fragment_path("hello.service");
    bump_mtime(&fragment);
    assert!(systemctl(&["daemon-reload"]));

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .expect("hello.service should appear")
        .clone();

    assert_eq!(
        svc["needs_daemon_reload"], false,
        "daemon-reload should have cleared the pending flag, svc: {svc}"
    );
    assert_eq!(
        svc["restart_pending"], true,
        "expected restart_pending after daemon-reload without restart, svc: {svc}"
    );
    assert!(
        !out.status.success(),
        "expected exit 1 when restart pending"
    );
}

#[test]
#[ignore]
fn status_detects_restart_loop_for_flapping_service() {
    // Build a oneshot unit that exits 1 the first three invocations (counter
    // on disk) and exits 0 on the fourth. With Type=oneshot
    // RemainAfterExit=yes plus Restart=on-failure RestartSec=100ms, systemd
    // cycles it four times in well under a second, ending in
    // ActiveState=active (held by RemainAfterExit) with NRestarts >= 3 and
    // a very small uptime since active-enter — exactly the conditions the
    // restart-loop heuristic looks for.
    let _ctx = SyncTestContext::new();

    let uid = unsafe { libc::getuid() };
    let counter_path = format!("/tmp/quadcd-test-flapper-{uid}.count");
    let _ = fs::remove_file(&counter_path);

    let unit = format!(
        r#"[Unit]
Description=Flapping test service for quadcd status restart-loop heuristic

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'f="{counter_path}"; n=$(cat "$f" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$f"; [ "$n" -ge 4 ]'
Restart=on-failure
RestartSec=100ms

[Install]
WantedBy={target}
"#,
        target = wanted_by()
    );

    prime_repo_via_service("flapper", &[("flap.service", unit.as_str())]);

    // Wait until the service has cycled enough times and finally settled
    // into the success branch (ActiveState=active with NRestarts>=3).
    wait_until(
        Duration::from_secs(20),
        "flap.service to settle active with NRestarts >= 3",
        || {
            let mut cmd = Command::new("systemctl");
            if is_user_mode() {
                cmd.arg("--user");
            }
            let out = cmd
                .args([
                    "show",
                    "-p",
                    "ActiveState",
                    "-p",
                    "NRestarts",
                    "flap.service",
                ])
                .output()
                .ok();
            let Some(out) = out else { return false };
            let s = String::from_utf8_lossy(&out.stdout);
            let active = s.lines().any(|l| l == "ActiveState=active");
            let n: u32 = s
                .lines()
                .find_map(|l| l.strip_prefix("NRestarts="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            active && n >= 3
        },
    );

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "flap.service")
        .expect("flap.service should appear in services")
        .clone();

    assert_eq!(svc["active_state"], "active", "svc: {svc}");
    assert!(
        svc["n_restarts"].as_u64().unwrap_or(0) >= 3,
        "expected NRestarts >= 3, svc: {svc}"
    );
    assert_eq!(
        svc["restart_loop_suspected"], true,
        "expected restart_loop_suspected=true, svc: {svc}"
    );
    assert!(
        !out.status.success(),
        "expected exit 1 when restart loop suspected"
    );

    let _ = fs::remove_file(&counter_path);
}
