//! Integration tests for `Podman` using a static fake command script.
//!
//! Uses `tests/fixtures/fake_cmd.sh`, controlled via environment variables
//! (`FAKE_EXIT_CODE`, `FAKE_STDOUT`, `FAKE_STDERR`).
//!
//! Arg-checking tests use exit code 1 so that podman's failure path includes
//! the fake stderr (which contains the echoed args) in its warning message.

mod common;

use std::path::PathBuf;

use std::time::Duration;

use common::{test_config, TestWriter};
use quadcd::sync::{ImagePuller, ImageRef, Podman};

fn fake_cmd() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_cmd.sh")
}

fn fake_podman(exit_code: i32) -> Podman {
    Podman::new()
        .command(fake_cmd().to_str().unwrap())
        .env("FAKE_EXIT_CODE", &exit_code.to_string())
}

fn fake_podman_sleeping(secs: u32) -> Podman {
    Podman::new()
        .command(fake_cmd().to_str().unwrap())
        .env("FAKE_SLEEP", &secs.to_string())
}

fn simple_image(name: &str) -> ImageRef {
    ImageRef {
        image: name.to_string(),
        auth_file: None,
        tls_verify: None,
    }
}

fn test_cfg_with_capture(verbose: bool) -> (quadcd::Config, TestWriter) {
    let err_buf = TestWriter::new();
    let cfg = test_config(&TestWriter::new(), &err_buf);
    let mut cfg = cfg;
    cfg.verbose = verbose;
    (cfg, err_buf)
}

#[test]
fn pull_success() {
    let podman = fake_podman(0);
    let (cfg, _) = test_cfg_with_capture(false);

    podman.pull(&simple_image("quay.io/podman/hello:latest"), &cfg);
    // No error output expected on success.
}

#[test]
fn pull_success_verbose() {
    let podman = fake_podman(0);
    let (cfg, err_buf) = test_cfg_with_capture(true);

    podman.pull(&simple_image("quay.io/podman/hello:latest"), &cfg);

    let stderr = err_buf.captured();
    assert!(stderr.contains("Pre-pulling image"));
    assert!(stderr.contains("Successfully pulled"));
}

// pull failures

#[test]
fn pull_failure_logs_warning() {
    let podman = fake_podman(1);
    let (cfg, err_buf) = test_cfg_with_capture(false);

    podman.pull(&simple_image("bad:image"), &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("Warning: failed to pull image"),
        "stderr: {stderr}"
    );
}

#[test]
fn pull_missing_binary_logs_warning() {
    let podman = Podman::new().command("/no/such/podman-binary");
    let (cfg, err_buf) = test_cfg_with_capture(false);

    podman.pull(&simple_image("quay.io/podman/hello:latest"), &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("failed to run podman pull"),
        "expected 'failed to run' warning, got: {stderr}"
    );
}

// auth and tls flags — exit 1 so podman logs stderr (which contains the args)

#[test]
fn pull_passes_authfile_flag() {
    let podman = fake_podman(1);
    let (cfg, err_buf) = test_cfg_with_capture(false);

    let image = ImageRef {
        image: "registry.example.com/app:v1".to_string(),
        auth_file: Some("/run/secrets/auth.json".to_string()),
        tls_verify: None,
    };
    podman.pull(&image, &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("--authfile") && stderr.contains("/run/secrets/auth.json"),
        "stderr: {stderr}"
    );
}

#[test]
fn pull_passes_tls_verify_flag() {
    let podman = fake_podman(1);
    let (cfg, err_buf) = test_cfg_with_capture(false);

    let image = ImageRef {
        image: "registry.example.com/app:v1".to_string(),
        auth_file: None,
        tls_verify: Some(false),
    };
    podman.pull(&image, &cfg);

    let stderr = err_buf.captured();
    assert!(stderr.contains("--tls-verify=false"), "stderr: {stderr}");
}

#[test]
fn pull_passes_all_flags() {
    let podman = fake_podman(1);
    let (cfg, err_buf) = test_cfg_with_capture(false);

    let image = ImageRef {
        image: "registry.example.com/app:v1".to_string(),
        auth_file: Some("/auth.json".to_string()),
        tls_verify: Some(true),
    };
    podman.pull(&image, &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("--authfile") && stderr.contains("--tls-verify=true"),
        "expected both flags, got: {stderr}"
    );
    assert!(stderr.contains("registry.example.com/app:v1"));
}

#[test]
fn pull_timeout_logs_warning() {
    let podman = fake_podman_sleeping(10);
    let (mut cfg, err_buf) = test_cfg_with_capture(false);
    cfg.podman_pull_timeout = Duration::from_secs(1);

    podman.pull(&simple_image("slow:image"), &cfg);

    let stderr = err_buf.captured();
    assert!(
        stderr.contains("Timed out") && stderr.contains("slow:image"),
        "expected timeout warning, got: {stderr}"
    );
}
