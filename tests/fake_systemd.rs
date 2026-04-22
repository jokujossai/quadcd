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
