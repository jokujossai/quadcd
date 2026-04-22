//! Smoke tests for the compiled `quadcd` binary.
//!
//! These verify that the binary entry point works end-to-end. Detailed
//! CLI-path coverage is in the unit tests in `src/lib.rs`.

use predicates::str::contains;

fn quadcd_cmd() -> assert_cmd::Command {
    assert_cmd::cargo_bin_cmd!("quadcd")
}

#[test]
fn version_flag() {
    quadcd_cmd()
        .arg("-version")
        .assert()
        .success()
        .stdout(contains("quadcd version"));
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    quadcd_cmd()
        .arg("foobar")
        .env_remove("SYSTEMD_SCOPE")
        .assert()
        .failure();
}

#[test]
fn generate_missing_args_exits_nonzero() {
    quadcd_cmd().arg("generate").assert().failure();
}

#[test]
fn sync_no_config_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    quadcd_cmd()
        .arg("sync")
        .env("HOME", tmp.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("QUADCD_CONFIG")
        .assert()
        .failure();
}

#[test]
fn generate_invalid_flag_exits_nonzero() {
    quadcd_cmd()
        .args(["generate", "--badarg"])
        .assert()
        .failure();
}

#[test]
fn no_args_shows_usage() {
    quadcd_cmd().assert().failure().stderr(contains("Usage:"));
}

#[test]
fn help_subcommand_succeeds() {
    quadcd_cmd()
        .arg("help")
        .assert()
        .success()
        .stderr(contains("Usage:"));
}

#[test]
fn version_subcommand_succeeds() {
    quadcd_cmd()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("quadcd version"));
}
