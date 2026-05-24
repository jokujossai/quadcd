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

/// Convenience: extract stdout as a String.
fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Convenience: extract stderr as a String.
fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Write a single-repo config pointing at a bare repo path.
fn write_single_repo_config(repo_name: &str, url: &str) {
    fs::write(
        config_path(),
        format!("[repositories.{repo_name}]\nurl = \"{url}\"\nbranch = \"main\"\n"),
    )
    .unwrap();
}

/// Run a one-shot `quadcd sync` to populate the data dir from configured
/// repos. Mirrors the sync tests' assumption that the sync subcommand can be
/// invoked directly (no service loop) and returns exit code 0 on success.
fn run_sync_once() -> std::process::Output {
    let mut args: Vec<&str> = vec!["sync", "--sync-only"];
    if is_user_mode() {
        args.push("--user");
    }
    run_quadcd(&args)
}

/// Parse `quadcd status --json` stdout into a `serde_json::Value`, asserting
/// the binary exited cleanly enough to produce JSON.
fn parse_status_json(out: &std::process::Output) -> serde_json::Value {
    let stdout = stdout_str(out);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "status --json did not produce valid JSON: {e}\nstdout:\n{stdout}\nstderr:\n{}",
            stderr_str(out)
        )
    })
}

// ---------------------------------------------------------------------------
// Repo state tests
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

    // Config references a bare repo URL but we never clone it. The data dir
    // is empty, so the repo is "missing".
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

#[test]
#[ignore]
fn status_up_to_date_after_sync_exits_zero() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());

    let sync_out = run_sync_once();
    assert!(
        sync_out.status.success(),
        "sync failed: {}",
        stderr_str(&sync_out)
    );

    let status_out = run_status(&["--no-fetch"]);
    let stdout = stdout_str(&status_out);
    assert!(
        status_out.status.success(),
        "expected exit 0, got stdout:\n{stdout}\nstderr:\n{}",
        stderr_str(&status_out)
    );
    assert!(stdout.contains("up-to-date"), "stdout: {stdout}");
    assert!(stdout.contains("myapp"), "stdout: {stdout}");
}

#[test]
#[ignore]
fn status_behind_after_upstream_commit_exits_one() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());

    assert!(run_sync_once().status.success());

    // Push a new commit upstream without syncing.
    push_commit(&bare, &[("extra.txt", "x\n")], "extra commit");

    // Default fetch should observe the new commit and report "behind 1".
    let out = run_status(&[]);
    let stdout = stdout_str(&out);
    assert!(!out.status.success(), "expected exit 1, stdout: {stdout}");
    assert!(stdout.contains("behind 1"), "stdout: {stdout}");
}

#[test]
#[ignore]
fn status_no_fetch_does_not_detect_upstream_change() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

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

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

    // Rewrite the config with a different URL. Real on-disk repo still points
    // at the original bare; quadcd should detect the mismatch without
    // attempting a fetch against the configured-but-wrong URL.
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
// JSON output tests
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn status_json_is_parseable_and_has_required_fields() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

    let out = run_status(&["--no-fetch", "--json"]);
    assert!(
        out.status.success(),
        "expected exit 0, stderr: {}",
        stderr_str(&out)
    );

    let v = parse_status_json(&out);
    assert!(v["mode"].is_string());
    assert!(v["repos"].is_array());
    assert!(v["services"].is_array());

    let repos = v["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"], "myapp");
    assert_eq!(repos[0]["branch"], "main");
    assert_eq!(repos[0]["state"]["state"], "up-to-date");
    assert!(repos[0]["head_sha"].is_string());

    // A plain `.service` unit is enumerated; whether systemd considers it
    // active depends on whether the generator/sync actually loaded it. We
    // only assert structural fields here.
    let services = v["services"].as_array().unwrap();
    let svc = services.iter().find(|s| s["unit"] == "hello.service");
    assert!(svc.is_some(), "expected hello.service in {services:?}");
    let svc = svc.unwrap();
    assert!(svc["enabled"].is_string());
    assert!(svc["active_state"].is_string());
    assert!(svc["needs_daemon_reload"].is_boolean());
    assert!(svc["restart_pending"].is_boolean());
    assert!(svc["restart_loop_suspected"].is_boolean());
    assert!(svc["n_restarts"].is_number());
}

#[test]
#[ignore]
fn status_json_behind_state_carries_commit_count() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

    push_commit(&bare, &[("a.txt", "1")], "one");
    push_commit(&bare, &[("b.txt", "2")], "two");

    let out = run_status(&["--json"]);
    assert!(!out.status.success());
    let v = parse_status_json(&out);
    let state = &v["repos"][0]["state"];
    assert_eq!(state["state"], "behind");
    assert_eq!(state["commits"], 2);
}

// ---------------------------------------------------------------------------
// Service state tests
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn status_service_reports_active_state_after_sync_and_reload() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

    // Sync installs the unit file and triggers daemon-reload + start.
    // Wait for the unit to have been started so ActiveEnterTimestamp is set.
    wait_for_unit_start("hello.service", Duration::from_secs(10));

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .expect("hello.service should appear in services");

    assert_eq!(svc["active_state"], "active", "svc: {svc}");
    // Type=oneshot RemainAfterExit=yes → ExecStart succeeded.
    assert_eq!(svc["result"], "success");
    // No daemon-reload pending immediately after sync.
    assert_eq!(svc["needs_daemon_reload"], false);
}

#[test]
#[ignore]
fn status_detects_daemon_reload_pending_when_unit_file_modified() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());
    wait_for_unit_start("hello.service", Duration::from_secs(10));

    // Locate the loaded fragment and bump its mtime past systemd's cached
    // load time. Touching the fragment is enough — systemd's
    // `NeedDaemonReload` property compares the inode timestamps.
    let mut show = Command::new("systemctl");
    if is_user_mode() {
        show.arg("--user");
    }
    let fragment_out = show
        .args(["show", "-p", "FragmentPath", "hello.service"])
        .output()
        .unwrap();
    let fragment = String::from_utf8_lossy(&fragment_out.stdout)
        .trim()
        .strip_prefix("FragmentPath=")
        .map(PathBuf::from)
        .expect("FragmentPath property missing");
    assert!(
        fragment.exists(),
        "expected fragment file to exist at {fragment:?}"
    );
    // `touch -c -m -d "+1 minute"` to ensure mtime strictly advances past the
    // value systemd cached. Fall back to a write-and-rewrite if `touch` isn't
    // available.
    let touched = Command::new("touch")
        .args(["-m", "-d", "+1 minute", fragment.to_str().unwrap()])
        .status()
        .ok()
        .is_some_and(|s| s.success());
    if !touched {
        // Fallback: rewrite the file with identical content. This bumps mtime.
        let content = fs::read_to_string(&fragment).unwrap();
        fs::write(&fragment, content).unwrap();
    }

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .expect("hello.service should appear");
    assert_eq!(
        svc["needs_daemon_reload"], true,
        "expected NeedDaemonReload=yes after bumping fragment mtime, svc: {svc}"
    );
    // Reload pending alone is enough to flip the exit code.
    assert!(!out.status.success(), "expected exit 1 when reload pending");
}

#[test]
#[ignore]
fn status_detects_restart_pending_after_reload_without_restart() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());
    wait_for_unit_start("hello.service", Duration::from_secs(10));

    // Bump the fragment mtime, then run daemon-reload (without restart) so
    // NeedDaemonReload clears but the running instance still predates the
    // new on-disk definition. This is the exact case `restart_pending`
    // catches.
    let mut show = Command::new("systemctl");
    if is_user_mode() {
        show.arg("--user");
    }
    let fragment_out = show
        .args(["show", "-p", "FragmentPath", "hello.service"])
        .output()
        .unwrap();
    let fragment = String::from_utf8_lossy(&fragment_out.stdout)
        .trim()
        .strip_prefix("FragmentPath=")
        .map(PathBuf::from)
        .expect("FragmentPath property missing");

    let _ = Command::new("touch")
        .args(["-m", "-d", "+1 minute", fragment.to_str().unwrap()])
        .status();

    assert!(systemctl(&["daemon-reload"]));

    let out = run_status(&["--no-fetch", "--json"]);
    let v = parse_status_json(&out);
    let svc = v["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["unit"] == "hello.service")
        .expect("hello.service should appear");

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
fn status_plain_output_has_expected_columns() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    write_single_repo_config("myapp", bare.to_str().unwrap());
    assert!(run_sync_once().status.success());

    let out = run_status(&["--no-fetch"]);
    let stdout = stdout_str(&out);
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
        "myapp",
    ] {
        assert!(
            stdout.contains(needle),
            "expected column/value '{needle}' in plain output, got:\n{stdout}"
        );
    }
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
    // Pre-clean any leftover counter from a prior run.
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
"#
    );

    let bare = create_bare_repo("flapper", &[("flap.service", unit.as_str())]);
    write_single_repo_config("flapper", bare.to_str().unwrap());

    assert!(run_sync_once().status.success());

    // Wait until the service has cycled enough times and finally settled
    // into the sleep-infinity branch (ActiveState=active with NRestarts>=3).
    // Polling NRestarts directly via systemctl is the most reliable signal.
    wait_until(
        Duration::from_secs(15),
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
        .expect("flap.service should appear in services");

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

    // Clean up the on-disk counter so a re-run of the test starts fresh.
    let _ = fs::remove_file(&counter_path);
}
