//! Integration tests for `GitVcs` using real git on local bare repos.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use quadcd::sync::GitVcs;
use quadcd::sync::Vcs;

/// Create a tempdir containing:
///   bare/   — a bare repo (the "remote")
///   clone/  — a clone of bare/ with one initial commit (a `.container` file)
///
/// Returns `(tempdir, bare_path, clone_path)`.
fn setup_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare");
    let clone = tmp.path().join("clone");

    // Create bare repo with explicit "main" branch
    run_git(&["init", "--bare", "-b", "main", bare.to_str().unwrap()]);

    // Clone it
    run_git(&["clone", bare.to_str().unwrap(), clone.to_str().unwrap()]);

    // Configure user in clone and set branch to main
    run_git_in(&clone, &["config", "user.email", "test@test.com"]);
    run_git_in(&clone, &["config", "user.name", "Test"]);
    run_git_in(&clone, &["checkout", "-b", "main"]);

    // Initial commit with a .container file
    std::fs::write(
        clone.join("app.container"),
        "Image=quay.io/podman/hello:latest\n",
    )
    .unwrap();
    run_git_in(&clone, &["add", "app.container"]);
    run_git_in(&clone, &["commit", "-m", "initial"]);
    run_git_in(&clone, &["push", "-u", "origin", "main"]);

    (tmp, bare, clone)
}

fn run_git(args: &[&str]) {
    let status = Command::new("git").args(args).status().unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

fn run_git_in(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} in {:?} failed", args, dir);
}

fn git_vcs() -> GitVcs {
    GitVcs::with_command(None, Duration::from_secs(60))
}

/// Clone `bare` into a second working copy, commit a file, and push to origin.
/// Returns the tempdir holding the second clone (must be kept alive).
fn push_from_second_clone(bare: &Path, filename: &str, message: &str) -> tempfile::TempDir {
    let tmp2 = tempfile::tempdir().unwrap();
    let clone2 = tmp2.path().join("clone2");
    run_git(&["clone", bare.to_str().unwrap(), clone2.to_str().unwrap()]);
    run_git_in(&clone2, &["config", "user.email", "test@test.com"]);
    run_git_in(&clone2, &["config", "user.name", "Test"]);
    std::fs::write(clone2.join(filename), format!("{filename}\n")).unwrap();
    run_git_in(&clone2, &["add", filename]);
    run_git_in(&clone2, &["commit", "-m", message]);
    run_git_in(&clone2, &["push", "origin", "main"]);
    tmp2
}

#[test]
fn check_succeeds() {
    assert!(git_vcs().check().is_ok());
}

#[test]
fn check_fails_bad_command() {
    let vcs = GitVcs::with_command(Some("/no/such/git-binary"), Duration::from_secs(60));
    assert!(vcs.check().is_err());
}

// clone_repo

#[test]
fn clone_repo_creates_git_dir() {
    let (_tmp, bare, _clone) = setup_repo();
    let target = tempfile::tempdir().unwrap();
    let dest = target.path().join("cloned");

    git_vcs()
        .clone_repo(bare.to_str().unwrap(), None, &dest)
        .unwrap();

    assert!(dest.join(".git").exists());
}

#[test]
fn clone_repo_with_branch() {
    let (_tmp, bare, clone) = setup_repo();

    // Create a "dev" branch
    run_git_in(&clone, &["checkout", "-b", "dev"]);
    std::fs::write(clone.join("dev.txt"), "dev\n").unwrap();
    run_git_in(&clone, &["add", "dev.txt"]);
    run_git_in(&clone, &["commit", "-m", "dev commit"]);
    run_git_in(&clone, &["push", "origin", "dev"]);

    let target = tempfile::tempdir().unwrap();
    let dest = target.path().join("cloned");

    git_vcs()
        .clone_repo(bare.to_str().unwrap(), Some("dev"), &dest)
        .unwrap();

    assert!(dest.join("dev.txt").exists());
}

#[test]
fn clone_repo_bad_url_errors() {
    let target = tempfile::tempdir().unwrap();
    let dest = target.path().join("cloned");

    let result = git_vcs().clone_repo("/no/such/repo.git", None, &dest);
    assert!(result.is_err());
}

#[test]
fn head_sha_returns_hex() {
    let (_tmp, _bare, clone) = setup_repo();
    let sha = git_vcs().head_sha(&clone).expect("should return SHA");
    assert_eq!(sha.len(), 40);
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn head_sha_nonexistent_returns_none() {
    let sha = git_vcs().head_sha(Path::new("/no/such/repo"));
    assert!(sha.is_none());
}

// `changed_files` only returns files whose extension matches a recognised
// systemd unit type (e.g. .service, .timer, .container, .volume, etc.)
// as defined by QUADLET_EXTENSIONS and SYSTEMD_EXTENSIONS in install.rs.

#[test]
fn changed_files_detects_unit_files() {
    let (_tmp, _bare, clone) = setup_repo();
    let sha1 = git_vcs().head_sha(&clone).unwrap();

    // Add a .service file
    std::fs::write(clone.join("web.service"), "[Service]\nExecStart=/bin/web\n").unwrap();
    run_git_in(&clone, &["add", "web.service"]);
    run_git_in(&clone, &["commit", "-m", "add service"]);
    let sha2 = git_vcs().head_sha(&clone).unwrap();

    let changed = git_vcs().changed_files(&clone, &sha1, &sha2);
    assert!(changed.contains(&"web.service".to_string()));
}

#[test]
fn changed_files_filters_non_units() {
    let (_tmp, _bare, clone) = setup_repo();
    let sha1 = git_vcs().head_sha(&clone).unwrap();

    // Add a non-unit file
    std::fs::write(clone.join("README.md"), "# Readme\n").unwrap();
    run_git_in(&clone, &["add", "README.md"]);
    run_git_in(&clone, &["commit", "-m", "add readme"]);
    let sha2 = git_vcs().head_sha(&clone).unwrap();

    let changed = git_vcs().changed_files(&clone, &sha1, &sha2);
    assert!(!changed.contains(&"README.md".to_string()));
}

#[test]
fn remote_url_returns_origin() {
    let (_tmp, bare, clone) = setup_repo();
    let url = git_vcs().remote_url(&clone).unwrap();
    assert_eq!(url, bare.to_str().unwrap());
}

#[test]
fn set_remote_url_changes_url() {
    let (_tmp, _bare, clone) = setup_repo();
    let new_url = "/tmp/other-remote.git";

    git_vcs().set_remote_url(&clone, new_url).unwrap();
    let url = git_vcs().remote_url(&clone).unwrap();
    assert_eq!(url, new_url);
}

#[test]
fn fetch_picks_up_new_commits() {
    let (_tmp, bare, clone) = setup_repo();
    let _tmp2 = push_from_second_clone(&bare, "new.txt", "new commit");

    let sha_before = git_vcs().head_sha(&clone).unwrap();
    git_vcs().fetch(&clone).unwrap();

    // origin/main should now differ from local HEAD
    let output = Command::new("git")
        .args(["-C", clone.to_str().unwrap(), "rev-parse", "origin/main"])
        .output()
        .unwrap();
    let origin_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_ne!(origin_sha, sha_before);
}

#[test]
fn reset_hard_moves_head() {
    let (_tmp, bare, clone) = setup_repo();
    let _tmp2 = push_from_second_clone(&bare, "reset.txt", "for reset");

    let sha_before = git_vcs().head_sha(&clone).unwrap();
    git_vcs().fetch(&clone).unwrap();
    git_vcs().reset_hard(&clone, "main").unwrap();
    let sha_after = git_vcs().head_sha(&clone).unwrap();

    assert_ne!(sha_before, sha_after);
    assert!(clone.join("reset.txt").exists());
}

// pull_ff_only

#[test]
fn pull_ff_only_fast_forwards() {
    let (_tmp, bare, clone) = setup_repo();
    let _tmp2 = push_from_second_clone(&bare, "ff.txt", "ff commit");

    let sha_before = git_vcs().head_sha(&clone).unwrap();
    git_vcs().pull_ff_only(&clone, "main").unwrap();
    let sha_after = git_vcs().head_sha(&clone).unwrap();

    assert_ne!(sha_before, sha_after);
    assert!(clone.join("ff.txt").exists());
}

#[test]
fn pull_ff_only_diverged_errors() {
    let (_tmp, bare, clone) = setup_repo();
    let _tmp2 = push_from_second_clone(&bare, "diverge.txt", "diverge");

    // Make a local commit in clone (diverging)
    std::fs::write(clone.join("local.txt"), "local\n").unwrap();
    run_git_in(&clone, &["add", "local.txt"]);
    run_git_in(&clone, &["commit", "-m", "local diverge"]);

    let result = git_vcs().pull_ff_only(&clone, "main");
    assert!(result.is_err());
}

#[test]
fn default_branch_returns_main() {
    let (_tmp, _bare, clone) = setup_repo();
    let branch = git_vcs().default_branch(&clone);
    assert_eq!(branch, "main");
}

#[test]
fn default_branch_returns_master_when_set() {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare");
    let clone = tmp.path().join("clone");

    // Create bare repo with "master" as default branch
    run_git(&["init", "--bare", "-b", "master", bare.to_str().unwrap()]);

    // Clone it (via GitVcs so set-head --auto runs)
    git_vcs()
        .clone_repo(bare.to_str().unwrap(), None, &clone)
        .unwrap();

    // Configure user and make an initial commit
    run_git_in(&clone, &["config", "user.email", "test@test.com"]);
    run_git_in(&clone, &["config", "user.name", "Test"]);
    std::fs::write(
        clone.join("app.container"),
        "Image=quay.io/podman/hello:latest\n",
    )
    .unwrap();
    run_git_in(&clone, &["add", "app.container"]);
    run_git_in(&clone, &["commit", "-m", "initial"]);
    run_git_in(&clone, &["push", "-u", "origin", "master"]);

    let branch = git_vcs().default_branch(&clone);
    assert_eq!(branch, "master");
}

#[test]
fn default_branch_without_origin_head() {
    let (_tmp, _bare, clone) = setup_repo();

    // Remove origin/HEAD so the first fallback path fails
    run_git_in(&clone, &["remote", "set-head", "origin", "-d"]);

    // Should fall back to symbolic-ref HEAD (the local checked-out branch)
    let branch = git_vcs().default_branch(&clone);
    assert_eq!(branch, "main");
}

#[test]
fn clone_repo_sets_origin_head() {
    let (_tmp, bare, _clone) = setup_repo();
    let target = tempfile::tempdir().unwrap();
    let dest = target.path().join("cloned");

    git_vcs()
        .clone_repo(bare.to_str().unwrap(), None, &dest)
        .unwrap();

    // Verify that refs/remotes/origin/HEAD was set by the post-clone set-head --auto
    let output = Command::new("git")
        .args([
            "-C",
            dest.to_str().unwrap(),
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "symbolic-ref refs/remotes/origin/HEAD should succeed after clone"
    );
    let symref = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(symref, "refs/remotes/origin/main");
}

// timeout

#[test]
fn git_timeout_kills_hung_process() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_git = tmp.path().join("fake-git");
    std::fs::write(&fake_git, "#!/bin/sh\nsleep 3600\n").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let vcs = GitVcs::with_command(Some(fake_git.to_str().unwrap()), Duration::from_secs(1));
    let result = vcs.check();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("timed out"), "err: {err}");
}

#[test]
fn git_sets_ssh_command_with_known_hosts() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ssh_cmd.txt");
    let fake_git = tmp.path().join("fake-git");
    std::fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nprintf '%s' \"$GIT_SSH_COMMAND\" > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let known_hosts = tmp.path().join(".known_hosts");
    let vcs = GitVcs::with_command(Some(fake_git.to_str().unwrap()), Duration::from_secs(10))
        .known_hosts(known_hosts.clone())
        .accept_new_host_keys(true);
    let _ = vcs.check();

    let contents = std::fs::read_to_string(&marker).expect("marker file should exist");
    let expected = format!(
        "ssh -o BatchMode=yes -o UserKnownHostsFile={} -o StrictHostKeyChecking=accept-new",
        known_hosts.display()
    );
    assert_eq!(contents, expected);
}

#[test]
fn git_sets_terminal_prompt_env() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("prompt_value.txt");
    let fake_git = tmp.path().join("fake-git");
    std::fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nprintf '%s' \"$GIT_TERMINAL_PROMPT\" > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let vcs = GitVcs::with_command(Some(fake_git.to_str().unwrap()), Duration::from_secs(10));
    // We don't care about the result — the script always exits 0 after writing.
    let _ = vcs.check();

    let contents = std::fs::read_to_string(&marker).expect("marker file should exist");
    assert_eq!(contents, "0", "GIT_TERMINAL_PROMPT should be set to 0");
}
