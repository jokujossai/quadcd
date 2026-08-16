//! Basic sync service integration tests: initial clone, config reload
//! (add/remove repos).
//!
//! All tests are `#[ignore]`d so `cargo test` on a dev machine skips them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::helpers::*;

#[test]
#[ignore]
fn service_initial_sync_clones_repo() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo("myapp", &[("hello.service", oneshot_unit().as_str())]);

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "hello.service", Duration::from_secs(10));

    let content =
        fs::read_to_string(PathBuf::from(data_dir()).join("myapp/hello.service")).unwrap();
    assert!(
        content.contains("ExecStart=/bin/true"),
        "unexpected content: {content}"
    );

    // quadcd should have started the new service after syncing
    wait_for_unit_start("hello.service", Duration::from_secs(10));

    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_config_reload_adds_repo() {
    let _ctx = SyncTestContext::new();

    let bare_a = create_bare_repo("repo-a", &[("a.service", oneshot_unit().as_str())]);
    let bare_b = create_bare_repo("repo-b", &[("b.service", oneshot_unit().as_str())]);

    // Start with only repo-a
    fs::write(
        config_path(),
        format!(
            "[repositories.repo-a]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare_a.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("repo-a", "a.service", Duration::from_secs(10));
    wait_for_unit_start("a.service", Duration::from_secs(10));
    assert!(
        !PathBuf::from(data_dir()).join("repo-b").exists(),
        "repo-b should not exist yet"
    );

    // Add repo-b to config — triggers file watcher
    fs::write(
        config_path(),
        format!(
            "[repositories.repo-a]\nurl = \"{}\"\ninterval = \"2s\"\n\n\
             [repositories.repo-b]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare_a.to_str().unwrap(),
            bare_b.to_str().unwrap()
        ),
    )
    .unwrap();

    wait_for_file("repo-b", "b.service", Duration::from_secs(10));
    wait_for_unit_start("b.service", Duration::from_secs(10));

    assert!(PathBuf::from(data_dir()).join("repo-a/a.service").exists());
    assert!(PathBuf::from(data_dir()).join("repo-b/b.service").exists());
    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_config_reload_removes_repo() {
    let _ctx = SyncTestContext::new();

    let bare_a = create_bare_repo("repo-a", &[("a.service", oneshot_unit().as_str())]);
    let bare_b = create_bare_repo("repo-b", &[("b.service", oneshot_unit().as_str())]);

    // Start with both repos using short intervals
    fs::write(
        config_path(),
        format!(
            "[repositories.repo-a]\nurl = \"{}\"\ninterval = \"1s\"\n\n\
             [repositories.repo-b]\nurl = \"{}\"\ninterval = \"1s\"\n",
            bare_a.to_str().unwrap(),
            bare_b.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("repo-a", "a.service", Duration::from_secs(10));
    wait_for_file("repo-b", "b.service", Duration::from_secs(10));

    let sha_before = head_sha(&PathBuf::from(data_dir()).join("repo-b"));

    // Push a new commit to repo-b's bare repo
    push_commit(&bare_b, &[("new.txt", "new content\n")], "new commit");

    // Remove repo-b from config
    fs::write(
        config_path(),
        format!(
            "[repositories.repo-a]\nurl = \"{}\"\ninterval = \"1s\"\n\n# repo-b removed\n",
            bare_a.to_str().unwrap()
        ),
    )
    .unwrap();

    // Wait for the config reload to be processed, then wait for repo-a to
    // complete at least two sync cycles (proving the service had time to
    // sync repo-b if it were still tracked).
    wait_until(
        Duration::from_secs(10),
        "repo-a to complete sync cycles after config change",
        || journal_contains("5s ago", "repo-a' is already up to date"),
    );

    let sha_after = head_sha(&PathBuf::from(data_dir()).join("repo-b"));
    assert_eq!(
        sha_before, sha_after,
        "repo-b should not have been synced after removal from config"
    );
    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_interval_pulls_updates() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo("myapp", &[("hello.service", oneshot_unit().as_str())]);

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"1s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "hello.service", Duration::from_secs(10));
    let sha_before = head_sha(&PathBuf::from(data_dir()).join("myapp"));

    // Push a new commit with an additional file
    push_commit(
        &bare,
        &[("new.service", "[Service]\nExecStart=/bin/true\n")],
        "add new service",
    );

    // Wait for the new file to appear (pulled on next interval)
    wait_for_file("myapp", "new.service", Duration::from_secs(10));

    let sha_after = head_sha(&PathBuf::from(data_dir()).join("myapp"));
    assert_ne!(
        sha_before, sha_after,
        "HEAD should have advanced after pull"
    );
    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_graceful_shutdown() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo("myapp", &[("hello.service", oneshot_unit().as_str())]);

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    // Wait for initial sync to complete
    wait_for_file("myapp", "hello.service", Duration::from_secs(10));

    // Get PID and send SIGTERM
    let pid = service_main_pid(SERVICE_NAME).expect("service should have a PID");
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -TERM should succeed");

    // Wait for the service to stop
    wait_until(Duration::from_secs(5), "service to stop", || {
        !is_service_active()
    });

    // Verify the shutdown message was logged
    assert!(
        journal_contains("10s ago", "Shutting down"),
        "expected shutdown log message"
    );
}

#[test]
#[ignore]
fn service_allows_concurrent_manual_sync() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo("myapp", &[("hello.service", oneshot_unit().as_str())]);

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    // Wait for initial sync so the service is running and between ticks
    wait_for_file("myapp", "hello.service", Duration::from_secs(10));

    // Manual `quadcd sync` should now succeed alongside the running service;
    // it waits briefly if the service happens to be mid-tick.
    let output = run_quadcd(&["sync", "-v"]);
    assert!(
        output.status.success(),
        "concurrent sync should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_syncs_configured_branch() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo_on_branch(
        "myapp",
        "develop",
        &[("dev.service", oneshot_unit().as_str())],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\nbranch = \"develop\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    // The file from the develop branch should appear
    wait_for_file("myapp", "dev.service", Duration::from_secs(10));

    let content = fs::read_to_string(PathBuf::from(data_dir()).join("myapp/dev.service")).unwrap();
    assert!(
        content.contains("ExecStart=/bin/true"),
        "unexpected content: {content}"
    );
    assert!(is_service_active(), "service should still be running");
}

#[test]
#[ignore]
fn service_sync_reports_failed_quadlet_container() {
    let _ctx = SyncTestContext::new();

    // Quadlet container referencing an image that cannot be pulled. The
    // generated <name>.service will fail during ExecStart, leaving the unit
    // in `failed` state — sync should surface this in its journal.
    let bare = create_bare_repo(
        "broken",
        &[(
            "broken.container",
            format!(
                "[Container]\n\
                 Image=localhost/quadcd-does-not-exist:nope\n\n\
                 [Service]\nRestart=no\n\n\
                 [Install]\nWantedBy={}\n",
                wanted_by()
            )
            .as_str(),
        )],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.broken]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("broken", "broken.container", Duration::from_secs(10));

    wait_until(
        Duration::from_secs(30),
        "sync to report failed broken.service",
        || journal_contains("60s ago", "broken.service: failed"),
    );
    assert!(
        journal_contains("60s ago", "service(s) failed after restart: broken.service"),
        "expected aggregated failure summary for broken.service"
    );

    assert!(is_service_active(), "sync service should still be running");
}

#[test]
#[ignore]
fn service_sync_reports_failed_service() {
    let _ctx = SyncTestContext::new();

    // Service whose ExecStart always exits non-zero — systemd leaves it in
    // `failed` state after start. No `Restart=` so it stays failed.
    let bare = create_bare_repo(
        "myapp",
        &[(
            "crash.service",
            format!(
                "[Service]\nType=simple\nExecStart=/bin/false\n\n[Install]\nWantedBy={}\n",
                wanted_by()
            )
            .as_str(),
        )],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "crash.service", Duration::from_secs(10));

    // The sync service should log the failed unit's state and an aggregated
    // failure summary after starting it.
    wait_until(
        Duration::from_secs(15),
        "sync to report failed crash.service",
        || journal_contains("30s ago", "crash.service: failed"),
    );
    assert!(
        journal_contains("30s ago", "service(s) failed after restart: crash.service"),
        "expected aggregated failure summary in sync journal"
    );

    assert!(is_service_active(), "sync service should still be running");
}

/// A volume unit without `[Install]` that is stopped should NOT be started
/// when its file is updated during sync.  Only explicitly enabled units
/// should be auto-started; generated/static units are dependency-activated.
#[test]
#[ignore]
fn service_sync_does_not_start_stopped_generated_unit() {
    let _ctx = SyncTestContext::new();

    // A volume without [Install] — systemd reports it as "generated".
    let bare = create_bare_repo(
        "myapp",
        &[
            ("data.volume", "[Volume]\nLabel=quadcd-test-data\n"),
            (
                "hello.container",
                "[Container]\n\
                 Image=docker.io/library/alpine:latest\n\
                 Exec=/bin/true\n\
                 Volume=data.volume:/data\n\n\
                 [Install]\nWantedBy=default.target\n",
            ),
        ],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "data.volume", Duration::from_secs(10));
    wait_for_file("myapp", "hello.container", Duration::from_secs(10));

    // Wait for the container (and thus the volume) to have been started.
    wait_for_unit_start("hello.service", Duration::from_secs(30));
    wait_for_unit_start("data-volume.service", Duration::from_secs(10));

    // Stop the volume unit manually.
    assert!(
        systemctl(&["stop", "data-volume.service"]),
        "failed to stop data-volume.service"
    );
    // Also stop the container so the volume isn't pulled back in.
    assert!(
        systemctl(&["stop", "hello.service"]),
        "failed to stop hello.service"
    );

    // Confirm it is stopped.
    assert!(
        !systemctl(&["is-active", "--quiet", "data-volume.service"]),
        "data-volume.service should be inactive after stop"
    );

    // Push an update to the volume file — the content changes but there
    // is still no [Install] section.
    push_commit(
        &bare,
        &[("data.volume", "[Volume]\nLabel=quadcd-test-data-v2\n")],
        "update volume label",
    );

    // Wait for the sync to pick up the new commit.
    wait_until(
        Duration::from_secs(15),
        "sync to pick up volume update",
        || {
            let content = fs::read_to_string(PathBuf::from(data_dir()).join("myapp/data.volume"));
            content
                .as_ref()
                .map(|c| c.contains("quadcd-test-data-v2"))
                .unwrap_or(false)
        },
    );

    // Give sync a moment to (incorrectly) start the unit if the bug is present.
    thread::sleep(Duration::from_secs(3));

    // The volume unit should still be stopped — sync must NOT start it.
    assert!(
        !systemctl(&["is-active", "--quiet", "data-volume.service"]),
        "data-volume.service should NOT have been started by sync (no [Install] and was stopped)"
    );

    assert!(is_service_active(), "sync service should still be running");
}

/// Template analogue of the test above: when a template unit file changes,
/// sync restarts the instances that are running and leaves a stopped instance
/// stopped. `systemctl list-units --all` reports loaded-but-inactive
/// instances too, so expanding a template must not be taken as licence to
/// start them.
#[test]
#[ignore]
fn service_sync_does_not_start_stopped_template_instance() {
    let _ctx = SyncTestContext::new();

    // A template without [Install]: instances run only when started by hand.
    let template = |version: &str| {
        format!(
            "[Unit]\nDescription=worker %i {version}\n\n\
             [Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n"
        )
    };

    let bare = create_bare_repo("myapp", &[("worker@.service", template("v1").as_str())]);

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "worker@.service", Duration::from_secs(10));

    // Start two instances by hand; nothing else wants them. Retry until the
    // generator has run and systemd knows the template.
    wait_until(
        Duration::from_secs(15),
        "worker@1.service to be startable",
        || systemctl(&["start", "worker@1.service"]),
    );
    wait_until(
        Duration::from_secs(15),
        "worker@2.service to be startable",
        || systemctl(&["start", "worker@2.service"]),
    );
    wait_for_unit_start("worker@1.service", Duration::from_secs(10));
    wait_for_unit_start("worker@2.service", Duration::from_secs(10));

    // Stop one of them, as an operator would.
    assert!(
        systemctl(&["stop", "worker@1.service"]),
        "failed to stop worker@1.service"
    );
    assert!(
        !systemctl(&["is-active", "--quiet", "worker@1.service"]),
        "worker@1.service should be inactive after stop"
    );

    let before = active_enter_timestamp("worker@2.service");

    push_commit(
        &bare,
        &[("worker@.service", template("v2").as_str())],
        "update worker template",
    );

    // Wait for the sync to pick up the new commit.
    wait_until(
        Duration::from_secs(15),
        "sync to pick up template update",
        || {
            fs::read_to_string(PathBuf::from(data_dir()).join("myapp/worker@.service"))
                .map(|c| c.contains("v2"))
                .unwrap_or(false)
        },
    );

    // The running instance is restarted: its ActiveEnterTimestamp moves.
    wait_until(
        Duration::from_secs(30),
        "worker@2.service to be restarted by sync",
        || active_enter_timestamp("worker@2.service") != before,
    );

    // Give sync a moment to (incorrectly) start the stopped instance.
    thread::sleep(Duration::from_secs(3));

    assert!(
        !systemctl(&["is-active", "--quiet", "worker@1.service"]),
        "worker@1.service should NOT have been started by sync (stopped instance of a changed template)"
    );
    assert!(
        systemctl(&["is-active", "--quiet", "worker@2.service"]),
        "worker@2.service should still be active after the restart"
    );

    assert!(is_service_active(), "sync service should still be running");
}

/// Guard for [`sync_starts_unit_wanted_by_target_with_queued_job`]: releases
/// the gate service and removes the hand-written units even if an assertion
/// panics first, since a blocked gate would otherwise hold a systemd job open
/// for the rest of the run.
struct QueuedJobTargetGuard {
    release_marker: String,
    unit_files: Vec<String>,
}

impl Drop for QueuedJobTargetGuard {
    fn drop(&mut self) {
        // Let the gate's ExecStart return so the target's job can complete.
        let _ = fs::write(&self.release_marker, "release\n");
        let _ = systemctl(&["stop", "quadcd-hold.target"]);
        let _ = systemctl(&["stop", "quadcd-gate.service"]);
        let _ = systemctl(&["stop", "holdme.service"]);
        let _ = fs::remove_file(&self.release_marker);
        for f in &self.unit_files {
            let _ = fs::remove_file(f);
        }
        let _ = systemctl(&["daemon-reload"]);
    }
}

/// A changed, inactive unit must be started when the target that wants it has
/// a queued start job.
///
/// This is the boot case. A target implicitly orders itself after the units it
/// wants, and its own job cannot complete until theirs have, so for the whole
/// early-boot window in which quadcd's first sync runs (from
/// `quadcd-sync.service`, `WantedBy=multi-user.target`/`default.target`) the
/// boot target reads `ActiveState=inactive`, `SubState=dead` with a queued
/// start job. Targets never report `activating`: their only states are
/// `inactive` and `active`. So a freshly cloned unit used to look unwanted —
/// its `[Install]` target was "not active" — and a fresh host started nothing
/// until its next boot.
///
/// The target is held with its job queued deterministically rather than by a
/// sleep: a gate service ordered before it blocks until the test writes a
/// marker file, so the window stays open for exactly as long as the test needs.
#[test]
#[ignore]
fn sync_starts_unit_wanted_by_target_with_queued_job() {
    let _ctx = SyncTestContext::new();

    let unit_dir = PathBuf::from(systemd_unit_dir());
    fs::create_dir_all(&unit_dir).unwrap();
    let uid = unsafe { libc::getuid() };
    let release_marker = format!("/tmp/quadcd-gate-release-{uid}");
    let gate_path = unit_dir.join("quadcd-gate.service");
    let target_path = unit_dir.join("quadcd-hold.target");

    let _guard = QueuedJobTargetGuard {
        release_marker: release_marker.clone(),
        unit_files: vec![
            gate_path.to_string_lossy().into_owned(),
            target_path.to_string_lossy().into_owned(),
        ],
    };

    let _ = fs::remove_file(&release_marker);
    fs::write(
        &gate_path,
        format!(
            "[Unit]\nDescription=quadcd test gate\n\n\
             [Service]\nType=oneshot\nRemainAfterExit=yes\nTimeoutStartSec=300\n\
             ExecStart=/bin/sh -c 'while [ ! -e {release_marker} ]; do sleep 0.1; done'\n"
        ),
    )
    .unwrap();
    // `Wants=`/`After=` on the target rather than `[Install]` on the gate, so
    // no `systemctl enable` is needed. The `After=` is what a boot target adds
    // implicitly for everything it wants; spelling it out keeps the test from
    // depending on that implicit behaviour.
    fs::write(
        &target_path,
        "[Unit]\nDescription=quadcd hold target\n\
         Wants=quadcd-gate.service\nAfter=quadcd-gate.service\n",
    )
    .unwrap();
    assert!(systemctl(&["daemon-reload"]), "daemon-reload failed");

    // --no-block: the start returns immediately, leaving the job queued.
    assert!(
        systemctl(&["start", "--no-block", "quadcd-hold.target"]),
        "failed to queue quadcd-hold.target"
    );
    wait_until(
        Duration::from_secs(30),
        "quadcd-hold.target to have a queued start job",
        || has_queued_start_job("quadcd-hold.target"),
    );
    // The state that made the old check fail: the target is *not* running and
    // not `activating`, it is plain `inactive` with a job attached.
    assert_eq!(
        active_state("quadcd-hold.target"),
        "inactive",
        "a target waiting on its ordering dependencies should be inactive"
    );

    // A unit installed into the held target only — nothing active wants it.
    let bare = create_bare_repo(
        "holding",
        &[(
            "holdme.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n\
             [Install]\nWantedBy=quadcd-hold.target\n",
        )],
    );
    fs::write(
        config_path(),
        format!(
            "[repositories.holding]\nurl = \"{}\"\ninterval = \"60s\"\n",
            bare.to_str().unwrap()
        ),
    )
    .unwrap();

    // One-shot sync so the whole reload/activate cycle has finished by the
    // time the assertions run.
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

    // The job must still have been queued while sync ran, or the test would
    // prove nothing.
    assert!(
        has_queued_start_job("quadcd-hold.target"),
        "the gate should still be holding the target's job, stderr: {stderr}"
    );
    // Assert on quadcd's own decision, not only the resulting unit state, so
    // the test cannot pass because systemd happened to pull the unit in.
    assert!(
        stderr.contains("Starting units: holdme.service"),
        "sync should start a unit wanted by a target with a queued start job, stderr: {stderr}"
    );
    assert!(
        was_unit_started("holdme.service"),
        "holdme.service should have been started, stderr: {stderr}"
    );
}
