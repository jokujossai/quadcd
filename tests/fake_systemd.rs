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
use quadcd::sync::{ActivationState, Systemd, SystemdTrait};
use rstest::rstest;

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

/// Records the argv of every fake systemctl run to a temp file.
///
/// The argv-echo trick the `reverse_deps` tests use only works when the method
/// under test parses whatever lands on stdout. Where the test has to supply
/// canned output instead, this is how the arguments are pinned.
struct ArgvLog(tempfile::TempDir);

impl ArgvLog {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn path(&self) -> PathBuf {
        self.0.path().join("argv")
    }

    fn systemd(&self, stdout: &str) -> Systemd {
        Systemd::with_command(fake_cmd().to_str().unwrap())
            .with_env("FAKE_EXIT_CODE", "0")
            .with_env("FAKE_STDOUT", stdout)
            .with_env("FAKE_ARGV_FILE", self.path().to_str().unwrap())
    }

    fn recorded(&self) -> Vec<String> {
        std::fs::read_to_string(self.path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
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

// activation_state

#[test]
fn activation_state_reports_active_state_and_sub_state() {
    let sd = fake_systemd_stdout("ActiveState=activating\nSubState=start", 0);
    let cfg = test_cfg(false, false);

    let state = sd.activation_state("myapp.service", &cfg);
    assert_eq!(state, ActivationState::new("activating", "start"));
    // `systemctl is-active --quiet` would exit non-zero here; the raw
    // ActiveState is what separates a unit starting from a failed one.
    assert!(!state.is_active());
    assert!(state.is_starting());
    assert!(!state.is_auto_restarting());
}

#[test]
fn activation_state_distinguishes_auto_restart_from_starting() {
    let sd = fake_systemd_stdout("ActiveState=activating\nSubState=auto-restart", 0);
    let cfg = test_cfg(false, false);

    let state = sd.activation_state("myapp.service", &cfg);
    assert!(state.is_starting());
    assert!(state.is_auto_restarting());
}

// `systemctl is-active` exits 0 for `reloading` and, since v254, `refreshing`
// as well as `active`; `is_active` replaced that call and must not narrow it.
#[rstest]
#[case::active("active", "running", true)]
#[case::reloading("reloading", "reload", true)]
#[case::refreshing("refreshing", "refresh", true)]
#[case::activating("activating", "start", false)]
#[case::inactive("inactive", "dead", false)]
#[case::failed("failed", "failed", false)]
fn activation_state_is_active_matches_systemctl_is_active(
    #[case] active_state: &str,
    #[case] sub_state: &str,
    #[case] expected: bool,
) {
    let sd = fake_systemd_stdout(
        &format!("ActiveState={active_state}\nSubState={sub_state}"),
        0,
    );
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.activation_state("myapp.service", &cfg).is_active(),
        expected
    );
}

#[test]
fn activation_state_unknown_on_command_failure() {
    // Valid output, non-zero exit: without the success check the parse would
    // happily report the unit as active.
    let sd = fake_systemd_stdout("ActiveState=active\nSubState=running", 1);
    let cfg = test_cfg(false, false);

    let state = sd.activation_state("myapp.service", &cfg);
    assert_eq!(state, ActivationState::unknown());
    assert!(!state.is_active());
    assert!(!state.is_starting());
}

#[test]
fn activation_state_queries_only_the_two_properties() {
    let argv = ArgvLog::new();
    let sd = argv.systemd("ActiveState=active\nSubState=running");
    let cfg = test_cfg(false, true);

    sd.activation_state("myapp.service", &cfg);

    assert_eq!(
        argv.recorded(),
        vec!["--user show myapp.service --property=ActiveState --property=SubState".to_string()],
        "activation_state must stay a two-property query, in the right systemd scope"
    );
}

// pending_start_jobs

/// Real `systemctl list-jobs` output, captured from the containerized test
/// environment while a target waited on a blocked gate service — legend,
/// blank line, footer and all.
const LIST_JOBS_DECORATED: &str = "JOB UNIT                  TYPE  STATE\n\
     208 probe-implicit.target start waiting\n\
     175 probe-explicit.target start waiting\n\
     48  quadcd-test.service   start running\n\
     176 probe-gate1.service   start running\n\
     209 probe-gate2.service   start running\n\
     \n\
     5 jobs listed.";

/// The same listing in the shape production actually sees: `--no-legend` is
/// always passed, so neither the header nor the footer is printed.
const LIST_JOBS_PLAIN: &str = "208 probe-implicit.target start waiting\n\
     175 probe-explicit.target start waiting\n\
     48  quadcd-test.service   start running";

#[test]
fn pending_start_jobs_parses_units_with_start_jobs() {
    let sd = fake_systemd_stdout(LIST_JOBS_PLAIN, 0);
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
    // Belt and braces for the decorations `--no-legend` is meant to suppress:
    // the header's type column reads `TYPE` and the footer line is too short,
    // so both drop out even if some systemd version prints them anyway.
    let sd = fake_systemd_stdout(LIST_JOBS_DECORATED, 0);
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.pending_start_jobs(&cfg),
        vec![
            "probe-implicit.target".to_string(),
            "probe-explicit.target".to_string(),
            "quadcd-test.service".to_string(),
            "probe-gate1.service".to_string(),
            "probe-gate2.service".to_string(),
        ]
    );
}

#[test]
fn pending_start_jobs_excludes_stop_and_reload_jobs() {
    // A unit on its way *down* must not look like one coming up.
    let sd = fake_systemd_stdout(
        "12 boot.target          start   waiting\n\
         13 multi-user.target    stop    waiting\n\
         14 app.service          reload  running\n\
         15 web.service          restart running",
        0,
    );
    let cfg = test_cfg(false, false);

    assert_eq!(
        sd.pending_start_jobs(&cfg),
        vec!["boot.target".to_string(), "web.service".to_string()]
    );
}

#[test]
fn pending_start_jobs_empty_on_command_failure() {
    // Parseable output with a non-zero exit: without the success check these
    // units would be reported as coming up.
    let sd = fake_systemd_stdout(LIST_JOBS_PLAIN, 1);
    let cfg = test_cfg(false, false);

    assert!(sd.pending_start_jobs(&cfg).is_empty());
}

#[test]
fn pending_start_jobs_queries_list_jobs_without_legend() {
    let argv = ArgvLog::new();
    let sd = argv.systemd(LIST_JOBS_PLAIN);
    let cfg = test_cfg(false, true);

    sd.pending_start_jobs(&cfg);

    assert_eq!(
        argv.recorded(),
        vec!["--user list-jobs --no-legend --no-pager".to_string()],
        "the parse relies on --no-legend, and user mode must not read the system manager"
    );
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
