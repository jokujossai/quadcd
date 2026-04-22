//! Integration tests that run inside a Podman container with real systemd.
//!
//! These tests exercise `quadcd generate` against real Podman and systemd,
//! verifying file installation, generator invocation, and systemd unit
//! generation end-to-end.
//!
//! All tests are `#[ignore]`d so `cargo test` on a dev machine skips them.
//! Inside the container they run via:
//!   quadcd-test --ignored --test-threads=1
//!
//! Teardown between tests:
//!   1. Stop generated services
//!   2. Remove quadcd sources and config
//!   3. Run `systemctl daemon-reload` (resets systemd state, cleans quadlet files)

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use rstest::*;

use crate::helpers::is_user_mode;

// ---------------------------------------------------------------------------
// Mode-aware paths
// ---------------------------------------------------------------------------

const QUADCD_BIN: &str = "/usr/local/bin/quadcd";

fn source_dir() -> String {
    if is_user_mode() {
        "/home/quadcd-test/.local/share/quadcd/local".to_string()
    } else {
        "/var/lib/quadcd/local".to_string()
    }
}

fn generator_dirs() -> (String, String, String) {
    if is_user_mode() {
        let uid = unsafe { libc::getuid() };
        (
            format!("/run/user/{uid}/systemd/generator"),
            format!("/run/user/{uid}/systemd/generator.early"),
            format!("/run/user/{uid}/systemd/generator.late"),
        )
    } else {
        (
            "/run/systemd/generator".to_string(),
            "/run/systemd/generator.early".to_string(),
            "/run/systemd/generator.late".to_string(),
        )
    }
}

fn quadlet_dir() -> String {
    let (normal, _, _) = generator_dirs();
    format!("{normal}/quadcd")
}

fn wanted_by() -> &'static str {
    if is_user_mode() {
        "default.target"
    } else {
        "multi-user.target"
    }
}

fn systemctl(args: &[&str]) -> bool {
    let mut cmd = Command::new("systemctl");
    if is_user_mode() {
        cmd.arg("--user");
    }
    cmd.args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Test context that manages quadcd source files and systemd state cleanup.
///
/// Setup: creates the source directory and installs test source files.
/// Teardown (Drop): stops generated services, removes sources, runs daemon-reload.
struct TestContext {
    source_dir: PathBuf,
    generated_services: Vec<String>,
}

impl TestContext {
    /// Install a source file into the quadcd source directory.
    ///
    /// Supports subdirectories: `install_source("sub/app.container", "...")`
    /// creates the `sub/` directory automatically.
    fn install_source(&mut self, name: &str, content: &str) {
        let dest = self.source_dir.join(name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).expect("create source subdir");
        }
        fs::write(&dest, content).expect("write source file");

        // Track expected generated service name for cleanup.
        if let Some(svc) = quadcd::install::generated_unit_name(name) {
            self.generated_services.push(svc);
        }
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        // 1. Stop any generated services
        for svc in &self.generated_services {
            let _ = systemctl(&["stop", svc]);
        }

        // 2. Remove all quadcd source files and subdirectories
        if self.source_dir.exists() {
            clean_dir(&self.source_dir);
        }

        // 3. Daemon-reload resets systemd state (should also clean quadlet files)
        let _ = systemctl(&["daemon-reload"]);
    }
}

/// Remove all files and subdirectories inside `dir`, but keep `dir` itself.
fn clean_dir(dir: &std::path::Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = fs::remove_dir_all(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[fixture]
fn ctx() -> TestContext {
    let sd = PathBuf::from(source_dir());
    fs::create_dir_all(&sd).expect("create source dir");

    // Pre-clean any leftover sources from a previous test
    clean_dir(&sd);

    let (normal, early, late) = generator_dirs();

    // Ensure generator dirs exist
    fs::create_dir_all(&normal).expect("create normal dir");
    fs::create_dir_all(&early).expect("create early dir");
    fs::create_dir_all(&late).expect("create late dir");

    TestContext {
        source_dir: sd,
        generated_services: Vec::new(),
    }
}

fn run_quadcd(args: &[&str]) -> std::process::Output {
    Command::new(QUADCD_BIN)
        .args(args)
        .output()
        .expect("failed to execute quadcd")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[rstest]
#[ignore]
fn generate_installs_quadlet_files(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        &format!("[Container]\nImage=quay.io/podman/hello:latest\n\n[Service]\nRestart=no\n\n[Install]\nWantedBy={}\n", wanted_by()),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let installed = PathBuf::from(quadlet_dir()).join("hello.container");
    assert!(
        installed.exists(),
        "hello.container not installed to {}",
        quadlet_dir()
    );

    let content = fs::read_to_string(&installed).unwrap();
    assert!(
        content.contains("quay.io/podman/hello:latest"),
        "content: {content}"
    );
}

#[rstest]
#[ignore]
fn generate_produces_systemd_units(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(output.status.success());

    let has_hello_unit = fs::read_dir(&normal)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("hello"));
    assert!(has_hello_unit, "no hello-related unit found in {normal}");
}

/// Quadlet types that use a suffix produce service units named {stem}-{type}.service.
/// .volume → mydata-volume.service, .network → mynet-network.service, etc.
#[rstest]
#[ignore]
fn generate_volume_produces_suffixed_unit(mut ctx: TestContext) {
    ctx.install_source(
        "mydata.volume",
        &format!("[Volume]\n\n[Install]\nWantedBy={}\n", wanted_by()),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let has_suffixed_unit = fs::read_dir(&normal).unwrap().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.contains("mydata-volume")
    });
    assert!(has_suffixed_unit, "expected mydata-volume unit in {normal}");

    // Ensure the unsuffixed name was NOT generated
    let has_unsuffixed = fs::read_dir(&normal).unwrap().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name == "mydata.service"
    });
    assert!(
        !has_unsuffixed,
        "volume should NOT produce mydata.service (should be mydata-volume.service)"
    );
}

#[rstest]
#[ignore]
fn generate_network_produces_suffixed_unit(mut ctx: TestContext) {
    ctx.install_source(
        "mynet.network",
        &format!("[Network]\n\n[Install]\nWantedBy={}\n", wanted_by()),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let has_suffixed_unit = fs::read_dir(&normal).unwrap().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.contains("mynet-network")
    });
    assert!(has_suffixed_unit, "expected mynet-network unit in {normal}");
}

#[rstest]
#[ignore]
fn generate_container_produces_unsuffixed_unit(mut ctx: TestContext) {
    ctx.install_source(
        "myapp.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(output.status.success());

    // .container should produce myapp.service (no suffix)
    let has_unsuffixed = fs::read_dir(&normal).unwrap().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name == "myapp.service"
    });
    assert!(
        has_unsuffixed,
        "container should produce myapp.service in {normal}"
    );
}

#[rstest]
#[ignore]
fn generate_verbose_output(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("[quadcd]"),
        "verbose output should contain [quadcd] prefix, got: {stderr}"
    );
}

#[rstest]
#[ignore]
fn generate_idempotent(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );

    let (normal, early, late) = generator_dirs();
    let args: Vec<&str> = vec!["generate", "-v", &normal, &early, &late];
    let output1 = run_quadcd(&args);
    assert!(
        output1.status.success(),
        "first run failed: {}",
        String::from_utf8_lossy(&output1.stderr)
    );

    let output2 = run_quadcd(&args);
    assert!(
        output2.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&output2.stderr)
    );
}

#[rstest]
#[ignore]
fn generate_no_sources_succeeds(ctx: TestContext) {
    // Source dir exists but is empty (fixture pre-cleans it)
    let _ = ctx;
    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "generate with no sources should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[rstest]
#[ignore]
fn daemon_reload_cleans_quadlet_files(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(output.status.success());

    let installed = PathBuf::from(quadlet_dir()).join("hello.container");
    assert!(
        installed.exists(),
        "hello.container should exist before cleanup"
    );

    // Remove sources and daemon-reload — quadlet files should be cleaned
    let sd = source_dir();
    for entry in fs::read_dir(&sd).unwrap().flatten() {
        fs::remove_file(entry.path()).ok();
    }
    assert!(systemctl(&["daemon-reload"]), "daemon-reload failed");

    // Re-run generate with empty sources — systemd cleared the dir on reload
    let output2 = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(output2.status.success());

    assert!(
        !installed.exists(),
        "hello.container should be cleaned after generate with empty sources"
    );
}

#[ignore]
#[test]
fn version_flag() {
    let output = run_quadcd(&["-version"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("quadcd version"), "stdout: {stdout}");
}

#[ignore]
#[test]
fn generate_missing_args_fails() {
    let output = run_quadcd(&["generate"]);
    assert!(
        !output.status.success(),
        "generate without args should fail"
    );
}

#[ignore]
#[test]
fn unknown_subcommand_fails() {
    let output = run_quadcd(&["nonexistent"]);
    assert!(!output.status.success(), "unknown subcommand should fail");
}

#[rstest]
#[ignore]
fn generate_envsubst_replaces_variables(mut ctx: TestContext) {
    // Write a .env file in the source directory
    fs::write(
        PathBuf::from(source_dir()).join(".env"),
        "MY_IMAGE=quay.io/podman/hello:latest\n",
    )
    .expect("write .env file");

    ctx.install_source(
        "hello.container",
        &format!(
            "[Container]\nImage=${{MY_IMAGE}}\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let installed = PathBuf::from(quadlet_dir()).join("hello.container");
    let content = fs::read_to_string(&installed).unwrap();
    assert!(
        content.contains("Image=quay.io/podman/hello:latest"),
        "content: {content}"
    );
    assert!(
        !content.contains("${MY_IMAGE}"),
        "variable should have been replaced"
    );
}

#[rstest]
#[ignore]
fn generate_installs_plain_service_files(mut ctx: TestContext) {
    ctx.install_source(
        "myapp.service",
        &format!(
            "[Service]\nType=oneshot\nExecStart=/bin/true\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Plain .service files go directly to normal dir (not the quadlet subdir)
    let installed = PathBuf::from(&normal).join("myapp.service");
    assert!(installed.exists(), "myapp.service not found in {normal}");

    let content = fs::read_to_string(&installed).unwrap();
    assert!(
        content.contains("ExecStart=/bin/true"),
        "unexpected content: {content}"
    );
}

#[rstest]
#[ignore]
fn generate_installs_timer_files(mut ctx: TestContext) {
    ctx.install_source(
        "cleanup.timer",
        "[Timer]\nOnCalendar=daily\n\n[Install]\nWantedBy=timers.target\n",
    );
    ctx.install_source(
        "cleanup.service",
        "[Service]\nType=oneshot\nExecStart=/bin/true\n",
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let timer = PathBuf::from(&normal).join("cleanup.timer");
    assert!(timer.exists(), "cleanup.timer not found in {normal}");

    let service = PathBuf::from(&normal).join("cleanup.service");
    assert!(service.exists(), "cleanup.service not found in {normal}");

    let content = fs::read_to_string(&timer).unwrap();
    assert!(
        content.contains("OnCalendar=daily"),
        "unexpected timer content: {content}"
    );
}

#[ignore]
#[test]
fn generate_with_quadcd_unit_dirs() {
    let alt_dir = "/tmp/quadcd-alt-sources";
    let _ = fs::remove_dir_all(alt_dir);
    fs::create_dir_all(alt_dir).unwrap();

    fs::write(
        PathBuf::from(alt_dir).join("alt.container"),
        format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    )
    .unwrap();

    let (normal, early, late) = generator_dirs();

    // Ensure generator dirs exist
    fs::create_dir_all(&normal).expect("create normal dir");
    fs::create_dir_all(&early).expect("create early dir");
    fs::create_dir_all(&late).expect("create late dir");

    let output = Command::new(QUADCD_BIN)
        .args(["generate", "-v", &normal, &early, &late])
        .env("QUADCD_UNIT_DIRS", alt_dir)
        .output()
        .expect("failed to execute quadcd");

    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The file from the alt source dir should be installed
    let installed = PathBuf::from(quadlet_dir()).join("alt.container");
    assert!(
        installed.exists(),
        "alt.container not installed from QUADCD_UNIT_DIRS"
    );

    // Clean up
    let _ = fs::remove_dir_all(alt_dir);
    // Daemon-reload to clear generator state
    let _ = systemctl(&["daemon-reload"]);
}

#[rstest]
#[ignore]
fn generate_installs_files_from_subdirectories(mut ctx: TestContext) {
    ctx.install_source(
        "group-a/app-a.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );
    ctx.install_source(
        "group-b/app-b.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Both files should be installed into the flat quadlet dir
    let qd = quadlet_dir();
    let app_a = PathBuf::from(&qd).join("app-a.container");
    let app_b = PathBuf::from(&qd).join("app-b.container");
    assert!(app_a.exists(), "app-a.container not found in {qd}");
    assert!(app_b.exists(), "app-b.container not found in {qd}");

    // Both should produce generated systemd units
    let has_a = fs::read_dir(&normal)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy() == "app-a.service");
    let has_b = fs::read_dir(&normal)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy() == "app-b.service");
    assert!(has_a, "app-a.service not generated in {normal}");
    assert!(has_b, "app-b.service not generated in {normal}");
}

#[rstest]
#[ignore]
fn generate_skips_dotgit_in_subdirectories(mut ctx: TestContext) {
    // Simulate a .git directory with a file that looks like a quadlet unit
    let git_dir = ctx.source_dir.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(
        git_dir.join("hidden.container"),
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    )
    .unwrap();

    // A real unit in a visible subdirectory
    ctx.install_source(
        "app/real.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let qd = quadlet_dir();
    let real = PathBuf::from(&qd).join("real.container");
    let hidden = PathBuf::from(&qd).join("hidden.container");
    assert!(real.exists(), "real.container should be installed");
    assert!(!hidden.exists(), ".git/hidden.container should be skipped");
}

#[rstest]
#[ignore]
fn generate_warns_on_duplicate_filenames(mut ctx: TestContext) {
    // Two subdirectories with the same filename
    ctx.install_source(
        "repo-a/dup.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );
    ctx.install_source(
        "repo-b/dup.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("duplicate Quadlet file 'dup.container'"),
        "expected duplicate warning, got: {stderr}"
    );
}

#[rstest]
#[ignore]
fn generate_warns_on_quadlet_systemd_collision(mut ctx: TestContext) {
    // A .container file that will generate app.service
    ctx.install_source(
        "quadlet/app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
    );
    // An explicit .service file with the same name
    ctx.install_source(
        "systemd/app.service",
        "[Service]\nType=oneshot\nExecStart=/bin/true\n",
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("generates 'app.service' which conflicts"),
        "expected collision warning, got: {stderr}"
    );
}

#[rstest]
#[ignore]
fn generate_sets_source_path_on_container_unit(mut ctx: TestContext) {
    ctx.install_source(
        "hello.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // daemon-reload so systemctl can see the generated unit
    assert!(systemctl(&["daemon-reload"]), "daemon-reload failed");

    let show = Command::new("systemctl")
        .args(if is_user_mode() {
            vec!["--user", "show", "hello.service", "--property=SourcePath"]
        } else {
            vec!["show", "hello.service", "--property=SourcePath"]
        })
        .output()
        .expect("systemctl show failed");
    let stdout = String::from_utf8_lossy(&show.stdout);
    let source_path = stdout.trim();

    let expected = format!("SourcePath={}/hello.container", source_dir());
    assert_eq!(
        source_path, expected,
        "expected {expected}, got: {source_path}"
    );
}

#[rstest]
#[ignore]
fn generate_sets_source_path_on_plain_service(mut ctx: TestContext) {
    ctx.install_source(
        "myplain.service",
        &format!(
            "[Service]\nType=oneshot\nExecStart=/bin/true\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(systemctl(&["daemon-reload"]), "daemon-reload failed");

    let show = Command::new("systemctl")
        .args(if is_user_mode() {
            vec!["--user", "show", "myplain.service", "--property=SourcePath"]
        } else {
            vec!["show", "myplain.service", "--property=SourcePath"]
        })
        .output()
        .expect("systemctl show failed");
    let stdout = String::from_utf8_lossy(&show.stdout);
    let source_path = stdout.trim();

    let expected = format!("SourcePath={}/myplain.service", source_dir());
    assert_eq!(
        source_path, expected,
        "expected {expected}, got: {source_path}"
    );
}

// ---------------------------------------------------------------------------
// Drop-in tests
// ---------------------------------------------------------------------------

fn dropins_base_dir() -> PathBuf {
    if is_user_mode() {
        PathBuf::from("/home/quadcd-test/.config/containers/systemd")
    } else {
        PathBuf::from("/etc/containers/systemd")
    }
}

/// Clean up a drop-in directory created during a test.
fn clean_dropin(name: &str) {
    let dir = dropins_base_dir().join(name);
    let _ = fs::remove_dir_all(&dir);
}

/// Install a drop-in conf file in the standard Quadlet drop-in location.
fn install_dropin(dropin_dir_name: &str, conf_name: &str, content: &str) {
    let dir = dropins_base_dir().join(dropin_dir_name);
    fs::create_dir_all(&dir).expect("create drop-in dir");
    fs::write(dir.join(conf_name), content).expect("write drop-in conf");
}

#[rstest]
#[ignore]
fn generate_applies_global_container_dropin(mut ctx: TestContext) {
    // Create a global container drop-in that adds Environment to [Service]
    install_dropin(
        "container.d",
        "10-test.conf",
        "[Service]\nEnvironment=DROPIN_TEST=applied\n",
    );

    ctx.install_source(
        "dropin-test.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The generated service should contain the drop-in environment variable
    let service_path = PathBuf::from(&normal).join("dropin-test.service");
    assert!(
        service_path.exists(),
        "dropin-test.service not found in {normal}"
    );
    let content = fs::read_to_string(&service_path).unwrap();
    assert!(
        content.contains("DROPIN_TEST=applied"),
        "global container drop-in was not applied. Generated content:\n{content}"
    );

    clean_dropin("container.d");
}

#[rstest]
#[ignore]
fn generate_applies_unit_specific_dropin(mut ctx: TestContext) {
    // Create a unit-specific drop-in
    install_dropin(
        "specific.container.d",
        "20-override.conf",
        "[Service]\nEnvironment=UNIT_SPECIFIC=yes\n",
    );

    ctx.install_source(
        "specific.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let service_path = PathBuf::from(&normal).join("specific.service");
    assert!(service_path.exists(), "specific.service not found");
    let content = fs::read_to_string(&service_path).unwrap();
    assert!(
        content.contains("UNIT_SPECIFIC=yes"),
        "unit-specific drop-in was not applied. Generated content:\n{content}"
    );

    clean_dropin("specific.container.d");
}

#[rstest]
#[ignore]
fn daemon_reload_preserves_dropin_source_files(mut ctx: TestContext) {
    // Create a global drop-in
    install_dropin(
        "container.d",
        "10-preserve.conf",
        "[Service]\nEnvironment=PRESERVE_TEST=yes\n",
    );

    ctx.install_source(
        "preserve.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let (normal, early, late) = generator_dirs();
    let output = run_quadcd(&["generate", "-v", &normal, &early, &late]);
    assert!(
        output.status.success(),
        "quadcd generate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the drop-in conf exists before reload
    let dropin_conf = dropins_base_dir().join("container.d/10-preserve.conf");
    assert!(
        dropin_conf.exists(),
        "drop-in conf should exist before daemon-reload"
    );

    // daemon-reload clears the generator output directory (including symlinks)
    // but must NOT delete the original drop-in files behind the symlinks
    assert!(systemctl(&["daemon-reload"]), "daemon-reload failed");

    // The original drop-in source files must still exist
    assert!(
        dropin_conf.exists(),
        "daemon-reload must not delete drop-in source files behind symlinks"
    );
    let content = fs::read_to_string(&dropin_conf).unwrap();
    assert!(
        content.contains("PRESERVE_TEST=yes"),
        "drop-in content was corrupted: {content}"
    );

    clean_dropin("container.d");
}

#[rstest]
#[ignore]
fn generate_dryrun_applies_dropin(mut ctx: TestContext) {
    install_dropin(
        "container.d",
        "10-dryrun.conf",
        "[Service]\nEnvironment=DRYRUN_DROPIN=applied\n",
    );

    ctx.install_source(
        "drytest.container",
        &format!(
            "[Container]\nImage=quay.io/podman/hello:latest\n\n[Install]\nWantedBy={}\n",
            wanted_by()
        ),
    );

    let output = run_quadcd(&["generate", "-v", "-dryrun"]);
    assert!(
        output.status.success(),
        "quadcd generate -dryrun failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("DRYRUN_DROPIN=applied"),
        "drop-in was not applied in dryrun output:\n{stdout}"
    );

    // Drop-in source files must survive the dryrun temp dir cleanup
    let dropin_conf = dropins_base_dir().join("container.d/10-dryrun.conf");
    assert!(
        dropin_conf.exists(),
        "dryrun must not delete drop-in source files"
    );

    clean_dropin("container.d");
}
