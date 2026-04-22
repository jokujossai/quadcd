use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::cd_config::RepoConfig;
use crate::config::Config;

use super::units::all_unit_files;
use super::Vcs;

/// Result of syncing a single repository.
#[derive(Debug)]
pub enum SyncStatus {
    /// Fresh clone — all unit files are new.
    Cloned,
    /// Pulled or force-updated — `changed_files` lists what changed.
    Updated { changed_files: Vec<String> },
    /// HEAD unchanged.
    AlreadyUpToDate,
}

/// Safely join a repository name to the data directory, rejecting path traversal.
///
/// Defense-in-depth: repo names are already validated at config parse time,
/// but this check catches any bypass.
pub(crate) fn safe_repo_dir(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let path = data_dir.join(name);
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!(
                "repository name '{name}' would escape data directory"
            ));
        }
    }
    // Second check: catches symlinks or platform-specific edge cases not
    // caught by the component scan above.
    if !path.starts_with(data_dir) {
        return Err(format!(
            "repository path '{}' is outside data directory '{}'",
            path.display(),
            data_dir.display()
        ));
    }
    Ok(path)
}

/// Result of syncing all configured repositories.
#[derive(Debug)]
pub struct SyncResult {
    /// Unit files that changed across all repos.
    pub changed_files: Vec<String>,
    /// Number of repositories that failed to sync.
    pub failures: usize,
}

/// Sync a single repository into `repo_dir` according to `repo_config`.
pub(crate) fn sync_repo_inner(
    vcs: &dyn Vcs,
    repo_dir: &Path,
    repo_config: &RepoConfig,
    cfg: &Config,
) -> Result<SyncStatus, String> {
    let force = cfg.force;
    let verbose = cfg.verbose;
    let git_dir = repo_dir.join(".git");

    if !git_dir.exists() {
        if verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Cloning {} into {}",
                repo_config.url,
                repo_dir.display()
            );
        }
        vcs.clone_repo(&repo_config.url, repo_config.branch.as_deref(), repo_dir)?;
        return Ok(SyncStatus::Cloned);
    }

    // Capture pre-sync HEAD
    let pre_sha = vcs.head_sha(repo_dir);

    // Check remote URL matches
    let current_url = vcs.remote_url(repo_dir)?;
    let mut url_changed = false;
    if current_url != repo_config.url {
        if !force {
            return Err(format!(
                "Remote URL mismatch for {}: expected '{}', got '{}'. Use --force to override.",
                repo_dir.display(),
                repo_config.url,
                current_url
            ));
        }
        url_changed = true;
        if verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Updating remote URL from '{}' to '{}'",
                current_url,
                repo_config.url
            );
        }
        vcs.set_remote_url(repo_dir, &repo_config.url)?;
    }

    let branch = repo_config
        .branch
        .clone()
        .unwrap_or_else(|| vcs.default_branch(repo_dir));

    if force {
        if verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Force syncing {} (branch: {})",
                repo_dir.display(),
                branch
            );
        }
        vcs.fetch(repo_dir)?;
        vcs.reset_hard(repo_dir, &branch)?;

        let post_sha = vcs.head_sha(repo_dir);
        let files = if url_changed {
            // Histories are unrelated after a URL change; git diff is unreliable.
            all_unit_files(repo_dir)
        } else {
            match (pre_sha.as_deref(), post_sha.as_deref()) {
                (Some(old), Some(new)) if old != new => vcs.changed_files(repo_dir, old, new),
                (Some(old), Some(new)) if old == new => return Ok(SyncStatus::AlreadyUpToDate),
                _ => all_unit_files(repo_dir),
            }
        };

        return Ok(SyncStatus::Updated {
            changed_files: files,
        });
    }

    // Normal pull --ff-only
    if verbose {
        let _ = writeln!(
            cfg.output.err(),
            "[quadcd] Pulling {} (branch: {})",
            repo_dir.display(),
            branch
        );
    }

    vcs.pull_ff_only(repo_dir, &branch)?;

    let post_sha = vcs.head_sha(repo_dir);
    match (pre_sha.as_deref(), post_sha.as_deref()) {
        (Some(old), Some(new)) if old == new => Ok(SyncStatus::AlreadyUpToDate),
        (Some(old), Some(new)) => {
            let files = vcs.changed_files(repo_dir, old, new);
            Ok(SyncStatus::Updated {
                changed_files: files,
            })
        }
        _ => Ok(SyncStatus::Updated {
            changed_files: all_unit_files(repo_dir),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cd_config::RepoConfig;
    use crate::config::test_config;
    use rstest::fixture;
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;

    use super::super::vcs::testing::MockVcs;

    struct SyncRepoFixture {
        _tmp: tempfile::TempDir,
        repo_dir: std::path::PathBuf,
        vcs: MockVcs,
        cfg: Config,
        rc: RepoConfig,
        err_buf: crate::output::tests::TestWriter,
    }

    impl SyncRepoFixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let repo_dir = tmp.path().join("myrepo");
            fs::create_dir_all(repo_dir.join(".git")).unwrap();
            let err_buf = crate::output::tests::TestWriter::new();
            Self {
                _tmp: tmp,
                repo_dir,
                vcs: MockVcs::new(),
                cfg: test_config(Box::new(Vec::new()), Box::new(err_buf.clone())),
                rc: RepoConfig {
                    url: "https://example.com/repo.git".to_string(),
                    branch: None,
                    interval: None,
                },
                err_buf,
            }
        }

        fn no_git_dir(&self) -> &Self {
            fs::remove_dir(self.repo_dir.join(".git")).unwrap();
            self
        }

        fn sync(&self) -> Result<SyncStatus, String> {
            sync_repo_inner(&self.vcs, &self.repo_dir, &self.rc, &self.cfg)
        }
    }

    #[fixture]
    fn fixture() -> SyncRepoFixture {
        SyncRepoFixture::new()
    }

    // sync_repo_inner -- sync results

    #[rstest]
    fn sync_repo_fresh_clone(fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        let result = fixture.no_git_dir().sync().unwrap();
        assert!(matches!(result, SyncStatus::Cloned));
        assert_eq!(vcs.clone_called.borrow().len(), 1);
        assert_eq!(
            vcs.clone_called.borrow()[0].0,
            "https://example.com/repo.git"
        );
    }

    #[rstest]
    fn sync_repo_existing_up_to_date(fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("aaa".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("aaa".to_string());
        let result = fixture.sync().unwrap();

        assert!(matches!(result, SyncStatus::AlreadyUpToDate));
        assert_eq!(vcs.pull_called.borrow().as_slice(), &["main"]);
    }

    #[rstest]
    fn sync_repo_existing_updated(fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("old_sha".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new_sha".to_string());
        *vcs.changed_files_val.borrow_mut() = vec!["app.container".to_string()];
        let result = fixture.sync().unwrap();

        match result {
            SyncStatus::Updated { changed_files } => {
                assert_eq!(changed_files, vec!["app.container"]);
            }
            _ => panic!("expected Updated"),
        }
    }

    #[rstest]
    fn sync_repo_url_mismatch_without_force(fixture: SyncRepoFixture) {
        *fixture.vcs.remote_url_val.borrow_mut() = Ok("https://other.com/repo.git".to_string());
        let result = fixture.sync();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Remote URL mismatch"));
    }

    #[rstest]
    fn sync_repo_url_mismatch_with_force(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.remote_url_val.borrow_mut() = Ok("https://other.com/repo.git".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new_sha".to_string());
        fixture.cfg.force = true;

        let result = fixture.sync().unwrap();
        assert!(matches!(result, SyncStatus::Updated { .. }));
        assert_eq!(vcs.set_remote_url_called.borrow().len(), 1);
        assert!(*vcs.fetch_called.borrow());
    }

    #[rstest]
    fn sync_repo_force_mode(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
        *vcs.changed_files_val.borrow_mut() = vec!["x.service".to_string()];
        fixture.cfg.force = true;

        let result = fixture.sync().unwrap();
        match result {
            SyncStatus::Updated { changed_files } => {
                assert_eq!(changed_files, vec!["x.service"]);
            }
            _ => panic!("expected Updated"),
        }
        assert!(*vcs.fetch_called.borrow());
        assert_eq!(vcs.reset_hard_called.borrow().len(), 1);
    }

    #[rstest]
    fn sync_repo_with_branch(mut fixture: SyncRepoFixture) {
        fixture.rc.branch = Some("develop".to_string());

        let result = fixture.no_git_dir().sync().unwrap();

        assert!(matches!(result, SyncStatus::Cloned));
        assert_eq!(
            fixture.vcs.clone_called.borrow()[0].1,
            Some("develop".to_string())
        );
    }

    #[rstest]
    fn sync_repo_pull_uses_configured_branch(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("aaa".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("aaa".to_string());
        fixture.rc.branch = Some("develop".to_string());

        let result = fixture.sync().unwrap();
        assert!(matches!(result, SyncStatus::AlreadyUpToDate));
        assert_eq!(vcs.pull_called.borrow().as_slice(), &["develop"]);
    }

    // sync_repo_inner -- verbose

    #[rstest]
    fn sync_repo_clone_verbose(mut fixture: SyncRepoFixture) {
        fixture.cfg.verbose = true;

        let result = fixture.no_git_dir().sync().unwrap();
        let stderr = fixture.err_buf.captured();

        assert!(matches!(result, SyncStatus::Cloned));
        assert!(stderr.contains("Cloning"));
        assert!(stderr.contains("https://example.com/repo.git"));
    }

    #[rstest]
    fn sync_repo_pull_verbose(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("old".to_string());
        fixture.cfg.verbose = true;

        fixture.sync().unwrap();

        let stderr = fixture.err_buf.captured();
        assert!(stderr.contains("Pulling"));
    }

    #[rstest]
    fn sync_repo_force_verbose(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("old".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
        fixture.cfg.verbose = true;
        fixture.cfg.force = true;

        fixture.sync().unwrap();

        let stderr = fixture.err_buf.captured();
        assert!(stderr.contains("Force syncing"));
    }

    #[rstest]
    fn sync_repo_url_mismatch_force_verbose(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        fixture.rc.url = "https://new.com/repo.git".to_owned();
        *vcs.remote_url_val.borrow_mut() = Ok("https://old.com/repo.git".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("new".to_string());
        fixture.cfg.verbose = true;
        fixture.cfg.force = true;

        fixture.sync().unwrap();

        let stderr = fixture.err_buf.captured();
        assert!(stderr.contains("Updating remote URL"));
    }

    // sync_repo_inner -- sync errors

    #[rstest]
    fn sync_repo_pull_pre_sha_none(fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        fs::write(fixture.repo_dir.join("app.container"), "").unwrap();
        *vcs.head_sha_val.borrow_mut() = None;
        *vcs.post_pull_sha.borrow_mut() = Some("abc".to_string());

        let result = fixture.sync().unwrap();
        match result {
            SyncStatus::Updated { changed_files } => {
                assert!(changed_files.contains(&"app.container".to_string()));
            }
            _ => panic!("expected Updated with all_unit_files fallback"),
        }
    }

    #[rstest]
    fn sync_repo_force_sha_none(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        fs::write(fixture.repo_dir.join("web.service"), "").unwrap();

        *vcs.head_sha_val.borrow_mut() = None;
        *vcs.post_pull_sha.borrow_mut() = None;
        fixture.cfg.force = true;

        let result = fixture.sync().unwrap();
        match result {
            SyncStatus::Updated { changed_files } => {
                assert!(changed_files.contains(&"web.service".to_string()));
            }
            _ => panic!("expected Updated with all_unit_files fallback"),
        }
    }

    #[rstest]
    fn sync_repo_force_same_sha_is_up_to_date(mut fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.head_sha_val.borrow_mut() = Some("same".to_string());
        *vcs.post_pull_sha.borrow_mut() = Some("same".to_string());
        fixture.cfg.force = true;

        let result = fixture.sync().unwrap();
        assert!(matches!(result, SyncStatus::AlreadyUpToDate));
    }

    #[rstest]
    fn sync_repo_pull_error(fixture: SyncRepoFixture) {
        let vcs = &fixture.vcs;

        *vcs.remote_url_val.borrow_mut() = Err("git error".to_string());

        let result = fixture.sync();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("git error"));
    }

    // safe_repo_dir

    #[rstest]
    #[case::valid_name("myapp", Some("/var/lib/quadcd/myapp"))]
    #[case::dotdot_rejected("..", None)]
    #[case::traversal_rejected("../../etc", None)]
    fn safe_repo_dir_valid_name(#[case] name: &str, #[case] expected: Option<&str>) {
        let result = safe_repo_dir(Path::new("/var/lib/quadcd"), name);
        match result {
            Ok(path) => assert_eq!(path, PathBuf::from(expected.unwrap())),
            Err(e) => assert!(expected.is_none(), "expected error: {e}"),
        }
    }
}
