//! Integration tests for `Systemd` using a static fake command script.
//!
//! Uses `tests/fixtures/fake_cmd.sh` — a pre-existing script controlled via
//! environment variables (`FAKE_EXIT_CODE`, `FAKE_STDOUT`).

mod common;

use std::io;
use std::path::PathBuf;

use common::{test_config, TestWriter};
use quadcd::config::Config;
use quadcd::output::Output;
use quadcd::sync::{Systemd, SystemdTrait};

fn fake_cmd() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_cmd.sh")
}

fn fake_systemd(exit_code: i32) -> Systemd {
    Systemd::with_command(fake_cmd().to_str().unwrap())
        .with_env("FAKE_EXIT_CODE", &exit_code.to_string())
}

fn fake_systemd_stdout(stdout: &str, exit_code: i32) -> Systemd {
    Systemd::with_command(fake_cmd().to_str().unwrap())
        .with_env("FAKE_EXIT_CODE", &exit_code.to_string())
        .with_env("FAKE_STDOUT", stdout)
}

fn test_cfg(verbose: bool, user_mode: bool) -> Config {
    let mut cfg = Config::for_testing(Box::new(io::sink()), Box::new(io::sink()));
    cfg.verbose = verbose;
    cfg.is_user_mode = user_mode;
    cfg
}

fn test_cfg_with_capture(verbose: bool, user_mode: bool) -> (Config, TestWriter) {
    let err_buf = TestWriter::new();
    let mut cfg = test_config(&TestWriter::new(), &err_buf);
    cfg.verbose = verbose;
    cfg.is_user_mode = user_mode;
    (cfg, err_buf)
}

fn with_subprocess_capture(cfg: &mut Config) -> TestWriter {
    let sub_out = TestWriter::new();
    cfg.subprocess_output = Some(Output::new(
        Box::new(sub_out.clone()),
        Box::new(TestWriter::new()),
    ));
    sub_out
}

// daemon_reload

#[test]
fn daemon_reload_success() {
    let sd = fake_systemd(0);
    let mut cfg = test_cfg(false, false);
    let sub_out = with_subprocess_capture(&mut cfg);

    sd.daemon_reload(&cfg);

    let args = sub_out.captured();
    assert!(args.contains("daemon-reload"), "args: {args}");
}

#[test]
fn daemon_reload_failure_logs_error() {
    let sd = fake_systemd(1);
    let (cfg, err_buf) = test_cfg_with_capture(false, false);

    sd.daemon_reload(&cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("exited with") || stderr.contains("Failed to run"),
        "stderr: {stderr}"
    );
}

#[test]
fn daemon_reload_missing_binary_logs_error() {
    let sd = Systemd::with_command("/no/such/systemctl-binary");
    let (cfg, err_buf) = test_cfg_with_capture(false, false);

    sd.daemon_reload(&cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("Failed to run"),
        "expected 'Failed to run', got: {stderr}"
    );
}

#[test]
fn daemon_reload_user_mode_passes_user_flag() {
    let sd = fake_systemd(0);
    let mut cfg = test_cfg(false, true);
    let sub_out = with_subprocess_capture(&mut cfg);

    sd.daemon_reload(&cfg);

    let args = sub_out.captured();
    assert!(args.contains("--user"), "args: {args}");
}

#[test]
fn daemon_reload_verbose_logs() {
    let sd = fake_systemd(0);
    let (cfg, err_buf) = test_cfg_with_capture(true, false);

    sd.daemon_reload(&cfg);

    let stderr = err_buf.captured();
    assert!(stderr.contains("[quadcd] Running systemctl"));
}

#[test]
fn restart_success_verbose() {
    let sd = fake_systemd(0);
    let (cfg, err_buf) = test_cfg_with_capture(true, false);

    sd.restart(&["myapp.service".into()], &cfg);

    let stderr = err_buf.captured();
    assert!(stderr.contains("Restarted"), "stderr: {stderr}");
}

#[test]
fn restart_failure() {
    let sd = fake_systemd(1);
    let (cfg, err_buf) = test_cfg_with_capture(false, false);

    sd.restart(&["myapp.service".into()], &cfg);

    let stderr = err_buf.captured();
    assert!(stderr.contains("exited with") || stderr.contains("Failed to restart"));
}

#[test]
fn restart_missing_binary() {
    let sd = Systemd::with_command("/no/such/systemctl-binary");
    let (cfg, err_buf) = test_cfg_with_capture(false, false);

    sd.restart(&["myapp.service".into()], &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("Failed to restart"),
        "expected 'Failed to restart', got: {stderr}"
    );
}

#[test]
fn restart_user_mode() {
    let sd = fake_systemd(0);
    let mut cfg = test_cfg(false, true);
    let sub_out = with_subprocess_capture(&mut cfg);

    sd.restart(&["myapp.service".into()], &cfg);

    let args = sub_out.captured();
    assert!(args.contains("--user"), "args: {args}");
}

// is_enabled

#[test]
fn is_enabled_returns_state() {
    let sd = fake_systemd_stdout("enabled", 0);
    let cfg = test_cfg(false, false);

    assert_eq!(sd.is_enabled("myapp.service", &cfg), "enabled");
}

#[test]
fn is_enabled_error_returns_unknown() {
    let sd = Systemd::with_command("/no/such/systemctl-binary");
    let cfg = test_cfg(false, false);

    assert_eq!(sd.is_enabled("myapp.service", &cfg), "unknown");
}

// is_active

#[test]
fn is_active_success() {
    let sd = fake_systemd(0);
    let cfg = test_cfg(false, false);

    assert!(sd.is_active("myapp.service", &cfg));
}

#[test]
fn is_active_failure() {
    let sd = fake_systemd(3); // systemctl is-active returns 3 for inactive
    let cfg = test_cfg(false, false);

    assert!(!sd.is_active("myapp.service", &cfg));
}

// reverse_deps

#[test]
fn reverse_deps_queries_only_start_authorising_properties() {
    // Without FAKE_STDOUT the fake echoes its argv, so the parsed "deps" are
    // the systemctl arguments — enough to pin down which properties are asked
    // for. Property names must match systemd's spelling exactly: `systemctl
    // show` filters the D-Bus reply against the names asked for, so an
    // unrecognised name matches nothing — no line is emitted and the exit
    // status is still 0. A typo would silently look like "no reverse
    // dependencies", which is why the flags are pinned here.
    let sd = fake_systemd(0);
    let cfg = test_cfg(false, false);

    let args = sd.reverse_deps("myapp.service", &cfg);

    for included in ["WantedBy", "RequiredBy", "BoundBy", "UpheldBy"] {
        assert!(
            args.contains(&format!("--property={included}")),
            "{included} should authorise a start; got {args:?}"
        );
    }
    // `PartOf=` propagates stop/restart only, `Requisite=` never starts,
    // socket/timer activation is on demand, `Conflicts=` stops, and the
    // propagation properties carry stop and reload rather than start. Kept in
    // step with the sibling assertion in `sync::systemd`.
    for excluded in [
        "ConsistsOf",
        "RequisiteOf",
        "TriggeredBy",
        "ConflictedBy",
        "StopPropagatedFrom",
        "ReloadPropagatedFrom",
    ] {
        assert!(
            !args.contains(&format!("--property={excluded}")),
            "{excluded} must not authorise a start; got {args:?}"
        );
    }
}

#[test]
fn reverse_deps_unions_and_deduplicates_properties() {
    // One line per property, in systemd's own order: WantedBy, RequiredBy,
    // BoundBy (empty here), UpheldBy.
    let sd = fake_systemd_stdout(
        "multi-user.target default.target\ndefault.target\n\nsupervisor.service",
        0,
    );
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.reverse_deps("myapp.service", &cfg),
        vec![
            "multi-user.target".to_string(),
            "default.target".to_string(),
            "supervisor.service".to_string(),
        ]
    );
}

#[test]
fn reverse_deps_error_returns_empty() {
    let sd = Systemd::with_command("/no/such/systemctl-binary");
    let cfg = test_cfg(false, false);

    assert!(sd.reverse_deps("myapp.service", &cfg).is_empty());
}

// is_active_or_activating

#[test]
fn is_active_or_activating_true_for_activating() {
    // `systemctl is-active --quiet` would exit non-zero here; the ActiveState
    // reported by `systemctl show` is what makes the difference.
    let sd = fake_systemd_stdout("ActiveState=activating\nSubState=start", 0);
    let cfg = test_cfg(false, false);

    assert!(sd.is_active_or_activating("myapp.service", &cfg));
}

#[test]
fn is_active_or_activating_true_for_active() {
    let sd = fake_systemd_stdout("ActiveState=active\nSubState=running", 0);
    let cfg = test_cfg(false, false);

    assert!(sd.is_active_or_activating("myapp.target", &cfg));
}

#[test]
fn is_active_or_activating_false_for_inactive() {
    let sd = fake_systemd_stdout("ActiveState=inactive\nSubState=dead", 0);
    let cfg = test_cfg(false, false);

    assert!(!sd.is_active_or_activating("myapp.target", &cfg));
}

#[test]
fn is_active_or_activating_false_on_command_failure() {
    let sd = fake_systemd(1);
    let cfg = test_cfg(false, false);

    assert!(!sd.is_active_or_activating("myapp.target", &cfg));
}

// pending_start_jobs

/// Real `systemctl list-jobs` output, captured from the containerized test
/// environment while a target waited on a blocked service.
const LIST_JOBS_OUTPUT: &str = "JOB UNIT                  TYPE  STATE\n\
     208 probe-implicit.target start waiting\n\
     175 probe-explicit.target start waiting\n\
     48  quadcd-test.service   start running\n\
     \n\
     5 jobs listed.";

#[test]
fn pending_start_jobs_parses_units_with_start_jobs() {
    let sd = fake_systemd_stdout(LIST_JOBS_OUTPUT, 0);
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.pending_start_jobs(&cfg),
        vec![
            "probe-implicit.target".to_string(),
            "probe-explicit.target".to_string(),
            "quadcd-test.service".to_string(),
        ]
    );
}

#[test]
fn pending_start_jobs_ignores_legend_and_footer() {
    // The parse must not depend on `--no-legend` stripping the decorations:
    // the header's type column reads `TYPE` and the footer is too short.
    let sd = fake_systemd_stdout(LIST_JOBS_OUTPUT, 0);
    let cfg = test_cfg(false, false);

    let jobs = sd.pending_start_jobs(&cfg);
    assert!(!jobs.iter().any(|u| u == "UNIT"), "jobs: {jobs:?}");
    assert!(!jobs.iter().any(|u| u == "jobs"), "jobs: {jobs:?}");
}

#[test]
fn pending_start_jobs_excludes_stop_and_reload_jobs() {
    // A unit on its way *down* must not look like one coming up.
    let sd = fake_systemd_stdout(
        "12 shutdown.target      start   waiting\n\
         13 multi-user.target    stop    waiting\n\
         14 app.service          reload  running\n\
         15 web.service          restart running",
        0,
    );
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.pending_start_jobs(&cfg),
        vec!["shutdown.target".to_string(), "web.service".to_string()]
    );
}

#[test]
fn pending_start_jobs_empty_on_command_failure() {
    let sd = fake_systemd(1);
    let cfg = test_cfg(false, false);

    assert!(sd.pending_start_jobs(&cfg).is_empty());
}

#[test]
fn list_units_matching_parses_output() {
    let sd = fake_systemd_stdout(
        "foo@web.service  loaded active running Foo Web\nfoo@worker.service  loaded active running Foo Worker",
        0,
    );
    let cfg = test_cfg(false, false);

    let units = sd.list_units_matching("foo@*.service", &cfg);
    assert_eq!(
        units,
        vec![
            "foo@web.service".to_string(),
            "foo@worker.service".to_string(),
        ]
    );
}
