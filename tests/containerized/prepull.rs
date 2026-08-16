//! Pre-pull integration test: verifies container images are pulled before
//! service start.
//!
//! All tests are `#[ignore]`d so `cargo test` on a dev machine skips them.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::helpers::*;

/// Guard that cleans up test files on drop, ensuring cleanup even if
/// assertions fail.
struct PrepullTestGuard {
    files: Vec<String>,
}

impl Drop for PrepullTestGuard {
    fn drop(&mut self) {
        for f in &self.files {
            let _ = fs::remove_file(f);
        }
    }
}

fn wanted_by() -> &'static str {
    if is_user_mode() {
        "default.target"
    } else {
        "multi-user.target"
    }
}

fn override_service_path() -> String {
    if is_user_mode() {
        "/home/quadcd-test/.config/systemd/user/prepull-test.service".to_string()
    } else {
        "/etc/systemd/system/prepull-test.service".to_string()
    }
}

#[test]
#[ignore]
fn service_pre_pulls_container_image() {
    let _ctx = SyncTestContext::new();

    let image = "quay.io/podman/hello:latest";
    let marker = "/tmp/prepull-marker";
    let check_path = if is_user_mode() {
        "/home/quadcd-test/.local/bin/prepull-check.sh"
    } else {
        "/usr/local/bin/prepull-check.sh"
    };
    let override_path = override_service_path();

    let _guard = PrepullTestGuard {
        files: vec![
            override_path.clone(),
            marker.to_string(),
            check_path.to_string(),
        ],
    };

    // Ensure the image is not cached from a prior run
    let _ = Command::new("podman").args(["rmi", "-f", image]).status();

    let bare = create_bare_repo(
        "prepull",
        &[(
            "prepull-test.container",
            &format!(
                "[Container]\nImage={image}\n\n\
                 [Service]\nRestart=no\n\n\
                 [Install]\nWantedBy={}\n",
                wanted_by()
            ),
        )],
    );

    // Write a check script that records whether the image was already
    // pulled when the service starts.
    if let Some(parent) = Path::new(check_path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let check_script = format!(
        "#!/bin/sh\n\
         if podman image exists {image}; then\n\
         \x20 echo PREPULLED > {marker}\n\
         else\n\
         \x20 echo NOT_PREPULLED > {marker}\n\
         fi\n"
    );
    fs::write(check_path, &check_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(check_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Override the Quadlet-generated service so systemd uses our unit
    // instead of the podman-generated one.
    if let Some(parent) = Path::new(&override_path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(
        &override_path,
        format!(
            "[Service]\nType=oneshot\nRemainAfterExit=yes\n\
             ExecStart={check_path}\n"
        ),
    )
    .unwrap();

    fs::write(
        config_path(),
        format!(
            "[repositories.prepull]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    // Wait for the marker file written by our check script
    wait_until(Duration::from_secs(30), "prepull marker to appear", || {
        Path::new(marker).exists()
    });

    let result = fs::read_to_string(marker).unwrap();
    assert_eq!(
        result.trim(),
        "PREPULLED",
        "image should have been pre-pulled before ExecStart ran"
    );

    // Double-check the image exists locally
    let status = Command::new("podman")
        .args(["image", "exists", image])
        .status()
        .unwrap();
    assert!(status.success(), "image should exist locally");
}

/// A container without `[Install]` is never started by sync (nothing wants
/// it), so its image must not be pre-pulled either — otherwise every sync
/// downloads an image nothing is about to run.
#[test]
#[ignore]
fn sync_skips_pre_pull_for_unit_that_stays_stopped() {
    let _ctx = SyncTestContext::new();

    let image = "quay.io/podman/hello:latest";

    // Ensure the image is not cached from a prior run
    let _ = Command::new("podman").args(["rmi", "-f", image]).status();

    // No [Install] section: nothing wants this unit, so sync must leave it
    // stopped.
    let bare = create_bare_repo(
        "noinstall",
        &[(
            "idle.container",
            &format!("[Container]\nImage={image}\nExec=/bin/true\n"),
        )],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.noinstall]\nurl = \"{}\"\ninterval = \"60s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    // One-shot sync so the whole reload/pull/activate cycle has finished by
    // the time the assertions run.
    let mut args: Vec<&str> = vec!["sync", "-v"];
    if is_user_mode() {
        args.push("--user");
    }
    let output = run_quadcd(&args);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "quadcd sync should succeed, stderr: {stderr}"
    );
    assert!(
        stderr.contains("Skipping image pre-pull for units that will not be activated"),
        "sync should report the skipped pre-pull, stderr: {stderr}"
    );

    assert!(
        !systemctl(&["is-active", "--quiet", "idle.service"]),
        "idle.service should not have been started"
    );
    let exists = Command::new("podman")
        .args(["image", "exists", image])
        .status()
        .unwrap()
        .success();
    assert!(
        !exists,
        "image should not be pre-pulled for a unit that stays stopped"
    );
}
