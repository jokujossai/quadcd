//! VCS trait and implementation backed by a git binary.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use subprocess::{Capture, Exec, Redirection};

use super::is_unit_file;

/// Unit files affected by a diff between two commits.
///
/// `changed` contains files present at the new tree (added, modified, renamed-to,
/// copied-to). `deleted` contains files that no longer exist (removed, renamed-from).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnitChanges {
    pub changed: Vec<String>,
    pub deleted: Vec<String>,
}

impl UnitChanges {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.deleted.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changed.len() + self.deleted.len()
    }

    /// Merge another set of changes into this one.
    pub fn extend(&mut self, other: UnitChanges) {
        self.changed.extend(other.changed);
        self.deleted.extend(other.deleted);
    }

    /// Construct from a list of present files with no deletions (e.g. fresh clone).
    pub fn from_present(changed: Vec<String>) -> Self {
        Self {
            changed,
            deleted: Vec::new(),
        }
    }
}

/// Abstraction over version control operations.
///
/// `GitVcs` shells out to git; tests can substitute a mock that operates purely
/// in-memory.
pub trait Vcs {
    fn check(&self) -> Result<(), String>;
    fn clone_repo(&self, url: &str, branch: Option<&str>, target: &Path) -> Result<(), String>;
    fn head_sha(&self, repo_dir: &Path) -> Option<String>;
    fn changed_files(&self, repo_dir: &Path, old_sha: &str, new_sha: &str) -> UnitChanges;
    fn remote_url(&self, repo_dir: &Path) -> Result<String, String>;
    fn set_remote_url(&self, repo_dir: &Path, url: &str) -> Result<(), String>;
    fn fetch(&self, repo_dir: &Path) -> Result<(), String>;
    fn reset_hard(&self, repo_dir: &Path, branch: &str) -> Result<(), String>;
    fn pull_ff_only(&self, repo_dir: &Path, branch: &str) -> Result<(), String>;
    fn default_branch(&self, repo_dir: &Path) -> String;
}

/// VCS implementation backed by a git binary.
///
/// Respects the `GIT_COMMAND` environment variable to override the git path.
/// All git commands are run with `GIT_TERMINAL_PROMPT=0` and a custom
/// `GIT_SSH_COMMAND` to prevent interactive credential prompts and isolate the
/// known_hosts file, with a configurable timeout to prevent indefinite hangs.
pub struct GitVcs {
    cmd: String,
    timeout: Duration,
    known_hosts: Option<PathBuf>,
    accept_new_host_keys: bool,
    interactive: bool,
}

impl GitVcs {
    /// Create a `GitVcs` with an optional command override and operation timeout.
    ///
    /// Pass `None` for `cmd` to use the default `"git"` binary.
    pub fn with_command(cmd: Option<&str>, timeout: Duration) -> Self {
        Self {
            cmd: cmd.unwrap_or("git").to_string(),
            timeout,
            known_hosts: None,
            accept_new_host_keys: false,
            interactive: false,
        }
    }

    /// Use a custom known_hosts file instead of the system default.
    pub fn known_hosts(mut self, path: PathBuf) -> Self {
        self.known_hosts = Some(path);
        self
    }

    /// Accept unknown host keys on first connect (TOFU model).
    pub fn accept_new_host_keys(mut self, accept: bool) -> Self {
        self.accept_new_host_keys = accept;
        self
    }

    /// Enable interactive mode: inherit stdin and allow SSH prompts.
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    fn ssh_command(&self) -> String {
        let mut cmd = "ssh".to_string();
        if !self.interactive {
            cmd.push_str(" -o BatchMode=yes");
        }
        if let Some(ref path) = self.known_hosts {
            cmd.push_str(&format!(" -o UserKnownHostsFile={}", path.display()));
        }
        if self.accept_new_host_keys {
            cmd.push_str(" -o StrictHostKeyChecking=accept-new");
        }
        cmd
    }

    fn exec(&self) -> Exec {
        let e = Exec::cmd(&self.cmd).env("GIT_SSH_COMMAND", self.ssh_command());
        if self.interactive {
            e
        } else {
            e.stdin(Redirection::Null).env("GIT_TERMINAL_PROMPT", "0")
        }
    }

    /// Run a git command, capturing output with a timeout.
    fn run_git(&self, args: &[&str], operation: &str) -> Result<Capture, String> {
        let mut job = self
            .exec()
            .args(args.iter().copied())
            .stdout(Redirection::Pipe)
            .stderr(Redirection::Pipe)
            .start()
            .map_err(|e| format!("Failed to run {operation}: {e}"))?;

        let io_result = job
            .communicate()
            .map_err(|e| format!("{operation}: {e}"))?
            .limit_time(self.timeout)
            .read();

        match io_result {
            Ok((stdout, stderr)) => {
                let exit_status = job
                    .wait()
                    .map_err(|e| format!("{operation} wait failed: {e}"))?;
                Ok(Capture {
                    stdout,
                    stderr,
                    exit_status,
                })
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                job.kill().ok();
                Err(format!(
                    "{operation} timed out after {}s",
                    self.timeout.as_secs()
                ))
            }
            Err(e) => Err(format!("{operation}: {e}")),
        }
    }
}

/// Parse `git diff --name-status` output into `UnitChanges`.
///
/// Each line is `STATUS\tPATH`, except renames/copies which are
/// `R<score>\tOLD\tNEW` / `C<score>\tOLD\tNEW`. For renames, the old path is
/// treated as deleted and the new path as changed; for copies, the old path is
/// untouched and only the new path is added.
fn parse_name_status(stdout: &str) -> UnitChanges {
    let mut changes = UnitChanges::default();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let status = match parts.next() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let kind = status.as_bytes()[0];
        match kind {
            b'D' => {
                if let Some(path) = parts.next() {
                    if is_unit_file(path) {
                        changes.deleted.push(path.to_string());
                    }
                }
            }
            b'R' => {
                let old = parts.next();
                let new = parts.next();
                if let Some(o) = old {
                    if is_unit_file(o) {
                        changes.deleted.push(o.to_string());
                    }
                }
                if let Some(n) = new {
                    if is_unit_file(n) {
                        changes.changed.push(n.to_string());
                    }
                }
            }
            b'C' => {
                // Source is untouched; only the new path is "added".
                let _old = parts.next();
                if let Some(n) = parts.next() {
                    if is_unit_file(n) {
                        changes.changed.push(n.to_string());
                    }
                }
            }
            _ => {
                if let Some(path) = parts.next() {
                    if is_unit_file(path) {
                        changes.changed.push(path.to_string());
                    }
                }
            }
        }
    }
    changes
}

impl Vcs for GitVcs {
    fn check(&self) -> Result<(), String> {
        match self.run_git(&["--version"], "git check") {
            Ok(o) if o.success() => Ok(()),
            Ok(o) => Err(format!(
                "git check failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(format!("git not found: {e}")),
        }
    }

    fn clone_repo(&self, url: &str, branch: Option<&str>, target: &Path) -> Result<(), String> {
        let target_str = target.to_string_lossy();
        let mut args = vec!["clone", url];
        if let Some(b) = branch {
            args.push("--branch");
            args.push(b);
        }
        args.push(&target_str);

        let output = self.run_git(&args, "git clone")?;
        if !output.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Best-effort: ensure refs/remotes/origin/HEAD is set so that
        // default_branch() can resolve it reliably. Failure is ignored because
        // the clone itself already succeeded and default_branch() falls back
        // gracefully, and the Vcs trait has no Output parameter to route a
        // warning through.
        let head_args = ["-C", &target_str, "remote", "set-head", "origin", "--auto"];
        let _ = self.run_git(&head_args, "git remote set-head");

        Ok(())
    }

    fn head_sha(&self, repo_dir: &Path) -> Option<String> {
        let dir = repo_dir.to_string_lossy();
        self.run_git(&["-C", &dir, "rev-parse", "HEAD"], "git rev-parse")
            .ok()
            .filter(|o| o.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn changed_files(&self, repo_dir: &Path, old_sha: &str, new_sha: &str) -> UnitChanges {
        let dir = repo_dir.to_string_lossy();
        let output = match self.run_git(
            &["-C", &dir, "diff", "--name-status", old_sha, new_sha],
            "git diff",
        ) {
            Ok(o) if o.success() => o,
            _ => return UnitChanges::default(),
        };

        parse_name_status(&String::from_utf8_lossy(&output.stdout))
    }

    fn remote_url(&self, repo_dir: &Path) -> Result<String, String> {
        let dir = repo_dir.to_string_lossy();
        let output = self.run_git(
            &["-C", &dir, "remote", "get-url", "origin"],
            "git remote get-url",
        )?;
        if !output.success() {
            return Err(format!(
                "git remote get-url failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn set_remote_url(&self, repo_dir: &Path, url: &str) -> Result<(), String> {
        let dir = repo_dir.to_string_lossy();
        let output = self.run_git(
            &["-C", &dir, "remote", "set-url", "origin", url],
            "git remote set-url",
        )?;
        if !output.success() {
            return Err(format!(
                "git remote set-url failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    fn fetch(&self, repo_dir: &Path) -> Result<(), String> {
        let dir = repo_dir.to_string_lossy();
        let output = self.run_git(&["-C", &dir, "fetch", "origin"], "git fetch")?;
        if !output.success() {
            return Err(format!(
                "git fetch failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    fn reset_hard(&self, repo_dir: &Path, branch: &str) -> Result<(), String> {
        let dir = repo_dir.to_string_lossy();
        let target = format!("origin/{branch}");
        let output = self.run_git(&["-C", &dir, "reset", "--hard", &target], "git reset")?;
        if !output.success() {
            return Err(format!(
                "git reset --hard failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    fn pull_ff_only(&self, repo_dir: &Path, branch: &str) -> Result<(), String> {
        let dir = repo_dir.to_string_lossy();
        let output = self.run_git(
            &["-C", &dir, "pull", "--ff-only", "origin", branch],
            "git pull",
        )?;
        if !output.success() {
            return Err(format!(
                "git pull --ff-only failed: {}. Use --force to override.",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    fn default_branch(&self, repo_dir: &Path) -> String {
        let dir = repo_dir.to_string_lossy();

        // Step 1: try refs/remotes/origin/HEAD (set by clone or set-head --auto)
        if let Ok(output) = self.run_git(
            &["-C", &dir, "symbolic-ref", "refs/remotes/origin/HEAD"],
            "git symbolic-ref origin/HEAD",
        ) {
            if output.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if let Some(branch) = s.strip_prefix("refs/remotes/origin/") {
                    return branch.to_string();
                }
            }
        }

        // Step 2: fall back to the local checked-out branch
        if let Ok(output) = self.run_git(
            &["-C", &dir, "symbolic-ref", "HEAD"],
            "git symbolic-ref HEAD",
        ) {
            if output.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if let Some(branch) = s.strip_prefix("refs/heads/") {
                    return branch.to_string();
                }
            }
        }

        // Step 3: genuine last resort
        "main".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ssh_command_default() {
        let vcs = GitVcs::with_command(None, Duration::from_secs(60));
        assert_eq!(vcs.ssh_command(), "ssh -o BatchMode=yes");
    }

    #[test]
    fn ssh_command_with_known_hosts() {
        let vcs = GitVcs::with_command(None, Duration::from_secs(60))
            .known_hosts(PathBuf::from("/data/.known_hosts"));
        assert_eq!(
            vcs.ssh_command(),
            "ssh -o BatchMode=yes -o UserKnownHostsFile=/data/.known_hosts"
        );
    }

    #[test]
    fn ssh_command_with_accept_new() {
        let vcs = GitVcs::with_command(None, Duration::from_secs(60)).accept_new_host_keys(true);
        assert_eq!(
            vcs.ssh_command(),
            "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
        );
    }

    #[test]
    fn ssh_command_with_known_hosts_and_accept_new() {
        let vcs = GitVcs::with_command(None, Duration::from_secs(60))
            .known_hosts(PathBuf::from("/data/.known_hosts"))
            .accept_new_host_keys(true);
        assert_eq!(
            vcs.ssh_command(),
            "ssh -o BatchMode=yes -o UserKnownHostsFile=/data/.known_hosts -o StrictHostKeyChecking=accept-new"
        );
    }

    #[test]
    fn parse_name_status_added_modified() {
        let out = "A\tapp.container\nM\tweb.service\n";
        let changes = parse_name_status(out);
        assert_eq!(
            changes.changed,
            vec!["app.container".to_string(), "web.service".to_string()]
        );
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn parse_name_status_deleted() {
        let out = "D\told.container\n";
        let changes = parse_name_status(out);
        assert!(changes.changed.is_empty());
        assert_eq!(changes.deleted, vec!["old.container".to_string()]);
    }

    #[test]
    fn parse_name_status_rename_old_deleted_new_added() {
        let out = "R100\told.container\tnew.container\n";
        let changes = parse_name_status(out);
        assert_eq!(changes.changed, vec!["new.container".to_string()]);
        assert_eq!(changes.deleted, vec!["old.container".to_string()]);
    }

    #[test]
    fn parse_name_status_copy_only_new() {
        let out = "C75\tsrc.container\tcopy.container\n";
        let changes = parse_name_status(out);
        assert_eq!(changes.changed, vec!["copy.container".to_string()]);
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn parse_name_status_filters_non_units() {
        let out = "A\tREADME.md\nM\tapp.container\nD\tnotes.txt\n";
        let changes = parse_name_status(out);
        assert_eq!(changes.changed, vec!["app.container".to_string()]);
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn parse_name_status_mixed() {
        let out =
            "A\tnew.service\nD\told.container\nM\tweb.container\nR100\tfrom.timer\tto.timer\n";
        let changes = parse_name_status(out);
        assert_eq!(
            changes.changed,
            vec![
                "new.service".to_string(),
                "web.container".to_string(),
                "to.timer".to_string(),
            ]
        );
        assert_eq!(
            changes.deleted,
            vec!["old.container".to_string(), "from.timer".to_string()]
        );
    }

    #[test]
    fn parse_name_status_empty() {
        let changes = parse_name_status("");
        assert!(changes.is_empty());
    }

    #[test]
    fn ssh_command_interactive() {
        let vcs = GitVcs::with_command(None, Duration::from_secs(60))
            .known_hosts(PathBuf::from("/data/.known_hosts"))
            .interactive(true);
        assert_eq!(
            vcs.ssh_command(),
            "ssh -o UserKnownHostsFile=/data/.known_hosts"
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::new_without_default)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;

    pub struct MockVcs {
        pub clone_called: RefCell<Vec<(String, Option<String>, String)>>,
        pub head_sha_val: RefCell<Option<String>>,
        pub remote_url_val: RefCell<Result<String, String>>,
        pub pull_called: RefCell<Vec<String>>,
        pub fetch_called: RefCell<bool>,
        pub reset_hard_called: RefCell<Vec<String>>,
        pub set_remote_url_called: RefCell<Vec<String>>,
        pub changed_files_val: RefCell<UnitChanges>,
        pub post_pull_sha: RefCell<Option<String>>,
        pub default_branch_val: String,
    }

    impl MockVcs {
        pub fn new() -> Self {
            Self {
                clone_called: RefCell::new(Vec::new()),
                head_sha_val: RefCell::new(Some("abc123".to_string())),
                remote_url_val: RefCell::new(Ok("https://example.com/repo.git".to_string())),
                pull_called: RefCell::new(Vec::new()),
                fetch_called: RefCell::new(false),
                reset_hard_called: RefCell::new(Vec::new()),
                set_remote_url_called: RefCell::new(Vec::new()),
                changed_files_val: RefCell::new(UnitChanges::default()),
                post_pull_sha: RefCell::new(Some("abc123".to_string())),
                default_branch_val: "main".to_string(),
            }
        }
    }

    impl Vcs for MockVcs {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
        fn clone_repo(&self, url: &str, branch: Option<&str>, target: &Path) -> Result<(), String> {
            self.clone_called.borrow_mut().push((
                url.to_string(),
                branch.map(|b| b.to_string()),
                target.display().to_string(),
            ));
            std::fs::create_dir_all(target.join(".git")).ok();
            Ok(())
        }
        fn head_sha(&self, _repo_dir: &Path) -> Option<String> {
            self.head_sha_val.borrow().clone()
        }
        fn changed_files(&self, _repo_dir: &Path, _old_sha: &str, _new_sha: &str) -> UnitChanges {
            self.changed_files_val.borrow().clone()
        }
        fn remote_url(&self, _repo_dir: &Path) -> Result<String, String> {
            self.remote_url_val.borrow().clone()
        }
        fn set_remote_url(&self, _repo_dir: &Path, url: &str) -> Result<(), String> {
            self.set_remote_url_called
                .borrow_mut()
                .push(url.to_string());
            Ok(())
        }
        fn fetch(&self, _repo_dir: &Path) -> Result<(), String> {
            *self.fetch_called.borrow_mut() = true;
            Ok(())
        }
        fn reset_hard(&self, _repo_dir: &Path, branch: &str) -> Result<(), String> {
            self.reset_hard_called.borrow_mut().push(branch.to_string());
            *self.head_sha_val.borrow_mut() = self.post_pull_sha.borrow().clone();
            Ok(())
        }
        fn pull_ff_only(&self, _repo_dir: &Path, branch: &str) -> Result<(), String> {
            self.pull_called.borrow_mut().push(branch.to_string());
            *self.head_sha_val.borrow_mut() = self.post_pull_sha.borrow().clone();
            Ok(())
        }
        fn default_branch(&self, _repo_dir: &Path) -> String {
            self.default_branch_val.clone()
        }
    }
}
