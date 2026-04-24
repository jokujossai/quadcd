//! Basic sync service integration tests: initial clone, config reload
//! (add/remove repos).
//!
//! All tests are `#[ignore]`d so `cargo test` on a dev machine skips them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::helpers::*;

#[test]
#[ignore]
fn service_initial_sync_clones_repo() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
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

    let bare_a = create_bare_repo(
        "repo-a",
        &[(
            "a.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    let bare_b = create_bare_repo(
        "repo-b",
        &[(
            "b.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );

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

    let bare_a = create_bare_repo(
        "repo-a",
        &[(
            "a.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    let bare_b = create_bare_repo(
        "repo-b",
        &[(
            "b.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );

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

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );

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

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
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

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
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
        &[(
            "dev.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
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
