use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn install_script_rewrites_sync_service_binary_paths_for_custom_bindir() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("fake-bin");
    let fixtures = temp.path().join("fixtures");
    let bindir = temp.path().join("custom-bin");
    let prefix = temp.path().join("systemd");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(&fixtures).unwrap();

    fs::write(fixtures.join("quadcd-linux-x86_64"), "fake-binary").unwrap();
    fs::write(
        fixtures.join("SHA256SUMS"),
        "ignored  quadcd-linux-x86_64\n",
    )
    .unwrap();
    fs::write(
        fixtures.join("quadcd-sync.service"),
        "[Unit]\nConditionFileIsExecutable=/usr/local/bin/quadcd\n\n[Service]\nExecStart=/usr/local/bin/quadcd sync --service\n",
    )
    .unwrap();
    fs::write(
        fixtures.join("quadcd-sync-user.service"),
        "[Unit]\nConditionFileIsExecutable=/usr/local/bin/quadcd\n\n[Service]\nExecStart=/usr/local/bin/quadcd sync --service --user\n",
    )
    .unwrap();

    write_executable(
        &fake_bin.join("id"),
        "#!/bin/sh\nif [ \"$1\" = \"-u\" ]; then\n  echo 0\nelse\n  /usr/bin/id \"$@\"\nfi\n",
    );
    write_executable(
        &fake_bin.join("uname"),
        "#!/bin/sh\nif [ \"$1\" = \"-m\" ]; then\n  echo x86_64\nelse\n  /usr/bin/uname \"$@\"\nfi\n",
    );
    write_executable(
        &fake_bin.join("curl"),
        "#!/bin/sh\nset -eu\nout=\"\"\nurl=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o)\n      out=\"$2\"\n      shift 2\n      ;;\n    -fsSL)\n      shift\n      ;;\n    *)\n      url=\"$1\"\n      shift\n      ;;\n  esac\ndone\ncase \"$url\" in\n  https://api.github.com/repos/jokujossai/quadcd/releases/latest)\n    printf '{\"tag_name\":\"v9.9.9\"}'\n    ;;\n  https://github.com/jokujossai/quadcd/releases/download/v9.9.9/SHA256SUMS)\n    cp \"$QUADCD_INSTALL_FIXTURES/SHA256SUMS\" \"$out\"\n    ;;\n  https://github.com/jokujossai/quadcd/releases/download/v9.9.9/quadcd-linux-x86_64)\n    cp \"$QUADCD_INSTALL_FIXTURES/quadcd-linux-x86_64\" \"$out\"\n    ;;\n  https://raw.githubusercontent.com/jokujossai/quadcd/v9.9.9/dist/quadcd-sync.service)\n    cp \"$QUADCD_INSTALL_FIXTURES/quadcd-sync.service\" \"$out\"\n    ;;\n  https://raw.githubusercontent.com/jokujossai/quadcd/v9.9.9/dist/quadcd-sync-user.service)\n    cp \"$QUADCD_INSTALL_FIXTURES/quadcd-sync-user.service\" \"$out\"\n    ;;\n  *)\n    echo \"unexpected curl url: $url\" >&2\n    exit 1\n    ;;\nesac\n",
    );
    write_executable(&fake_bin.join("sha256sum"), "#!/bin/sh\nexit 0\n");
    write_executable(
        &fake_bin.join("install"),
        "#!/bin/sh\nset -eu\nsrc=\"$2\"\ndst=\"$3\"\nmkdir -p \"$(dirname \"$dst\")\"\ncp \"$src\" \"$dst\"\nchmod 755 \"$dst\"\n",
    );
    write_executable(&fake_bin.join("systemctl"), "#!/bin/sh\nexit 0\n");

    let output = Command::new("sh")
        .arg("install.sh")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BINDIR", &bindir)
        .env("PREFIX", &prefix)
        .env("QUADCD_INSTALL_FIXTURES", &fixtures)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let expected_bin = bindir.join("quadcd");
    let system_service = fs::read_to_string(prefix.join("system/quadcd-sync.service")).unwrap();
    let user_service = fs::read_to_string(prefix.join("user/quadcd-sync.service")).unwrap();

    for service in [&system_service, &user_service] {
        assert!(
            service.contains(&format!(
                "ConditionFileIsExecutable={}",
                expected_bin.display()
            )),
            "content: {service}"
        );
        assert!(
            service.contains(&format!(
                "ExecStart={} sync --service",
                expected_bin.display()
            )),
            "content: {service}"
        );
        assert!(
            service.contains(&expected_bin.display().to_string()),
            "content: {service}"
        );
        assert!(
            !service.contains("/usr/local/bin/quadcd"),
            "content: {service}"
        );
    }
}
