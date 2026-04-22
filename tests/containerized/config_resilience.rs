//! Tests for config file resilience: invalid TOML, recovery after restore.
//!
//! All tests are `#[ignore]`d so `cargo test` on a dev machine skips them.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::helpers::*;

#[test]
#[ignore]
fn service_config_reload_invalid_keeps_current() {
    let _ctx = SyncTestContext::new();

    let bare = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );

    let valid_config = format!(
        "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
        bare.to_str().unwrap()
    );

    fs::write(config_path(), &valid_config).unwrap();

    start_sync_service();

    wait_for_file("myapp", "hello.service", Duration::from_secs(10));

    // Write invalid TOML — service should log a warning and keep running
    fs::write(config_path(), "not valid toml [[[").unwrap();

    // Wait for the service to process the invalid config (logs a warning)
    wait_until(
        Duration::from_secs(5),
        "service to log config warning",
        || journal_contains("5s ago", "keeping current config"),
    );
    assert!(
        is_service_active(),
        "service should survive invalid config reload"
    );

    // Restore valid config — service should recover
    fs::write(config_path(), &valid_config).unwrap();

    // Wait for the service to process the restored config
    wait_until(
        Duration::from_secs(5),
        "service to reload restored config",
        || journal_contains("3s ago", "Config file changed, reloading"),
    );
    assert!(
        is_service_active(),
        "service should still be running after config restored"
    );

    assert!(
        PathBuf::from(data_dir())
            .join("myapp/hello.service")
            .exists(),
        "original repo should still be intact"
    );
}

#[test]
#[ignore]
fn service_config_reload_url_change_warns() {
    let _ctx = SyncTestContext::new();

    let bare_a = create_bare_repo(
        "myapp",
        &[(
            "hello.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );
    let bare_b = create_bare_repo(
        "myapp-alt",
        &[(
            "alt.service",
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
        )],
    );

    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare_a.to_str().unwrap()
        ),
    )
    .unwrap();

    start_sync_service();

    wait_for_file("myapp", "hello.service", Duration::from_secs(10));

    // Change the URL to a different repo
    fs::write(
        config_path(),
        format!(
            "[repositories.myapp]\nurl = \"{}\"\ninterval = \"2s\"\n",
            bare_b.to_str().unwrap()
        ),
    )
    .unwrap();

    // Wait for the warning about URL change
    wait_until(
        Duration::from_secs(5),
        "service to log URL change warning",
        || journal_contains("5s ago", "URL changed for"),
    );

    // The old file should still be there (not replaced, since --force wasn't used)
    assert!(
        PathBuf::from(data_dir())
            .join("myapp/hello.service")
            .exists(),
        "original files should remain without --force"
    );
    assert!(
        !Path::new(data_dir()).join("myapp/alt.service").exists(),
        "new repo files should NOT appear without --force"
    );
    assert!(is_service_active(), "service should still be running");
}
