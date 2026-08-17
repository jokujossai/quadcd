//! `quadcd status` — diagnostic view of repository sync state and managed
//! systemd service state.
//!
//! This module is strictly read-only: it inspects the configured repositories
//! and managed systemd units, but never mutates them. The default flow runs
//! `git fetch` so "behind upstream" counts are accurate; pass `--no-fetch` to
//! skip the network call.

mod render;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cd_config::CDConfig;
use crate::config::Config;
use crate::sync::{
    all_unit_files, safe_repo_dir, unit_name_for_restart, SystemdTrait, UnitState, Vcs,
};

/// Suspected restart-loop heuristic threshold: at least this many restarts.
const RESTART_LOOP_MIN_RESTARTS: u32 = 3;
/// Suspected restart-loop heuristic: the unit's current uptime must be below
/// this for the loop heuristic to fire. Short uptime + high restart count
/// suggests systemd is restarting the service repeatedly.
const RESTART_LOOP_MAX_UPTIME: Duration = Duration::from_secs(60);

/// Knobs for `run_status`.
pub struct StatusOptions {
    pub no_fetch: bool,
    pub json: bool,
}

/// Aggregated status of every configured repo + every managed service.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub mode: Mode,
    pub config_path: Option<PathBuf>,
    pub repos: Vec<RepoStatus>,
    pub services: Vec<ServiceStatus>,
    /// Non-fatal observations that don't affect the exit code (e.g. duplicate
    /// unit names across repos, or a repo with local-only commits).
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    User,
    System,
}

#[derive(Debug, Serialize)]
pub struct RepoStatus {
    pub name: String,
    pub url: String,
    pub branch: String,
    pub local_path: PathBuf,
    pub state: RepoState,
    pub fetched: bool,
    pub head_sha: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RepoState {
    Missing,
    UpToDate,
    Ahead { commits: usize },
    Behind { commits: usize },
    Diverged { ahead: usize, behind: usize },
    UrlMismatch { configured: String, actual: String },
    Error { message: String },
}

impl RepoState {
    /// True if the state should drive a non-zero exit code. `Ahead` is treated
    /// as a warning, not a problem: on a dev `--user` machine local commits
    /// are normal, and a CD host that finds local commits should still
    /// distinguish that from "out of date" or "broken".
    pub fn is_problem(&self) -> bool {
        !matches!(self, RepoState::UpToDate | RepoState::Ahead { .. })
    }
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub unit: String,
    pub origin_repo: String,
    pub source_file: PathBuf,
    pub active_state: String,
    pub sub_state: String,
    pub result: String,
    pub enabled: String,
    pub needs_daemon_reload: bool,
    pub restart_pending: bool,
    pub n_restarts: u32,
    #[serde(serialize_with = "ser_optional_duration_secs")]
    pub uptime: Option<Duration>,
    pub restart_loop_suspected: bool,
}

impl ServiceStatus {
    pub fn is_problem(&self) -> bool {
        self.active_state != "active"
            || self.needs_daemon_reload
            || self.restart_pending
            || self.restart_loop_suspected
    }
}

fn ser_optional_duration_secs<S: serde::Serializer>(
    d: &Option<Duration>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => s.serialize_some(&d.as_secs()),
        None => s.serialize_none(),
    }
}

/// Return `1` if the report contains any problem (used as the process exit
/// code so `status` is usable from monitoring/cron checks); otherwise `0`.
pub fn report_exit_code(report: &StatusReport) -> i32 {
    let repo_problem = report.repos.iter().any(|r| r.state.is_problem());
    let service_problem = report.services.iter().any(|s| s.is_problem());
    if repo_problem || service_problem {
        1
    } else {
        0
    }
}

/// Read-only entrypoint: collect status from the configured repos and managed
/// services, render it (plain or JSON), and return an exit code.
pub fn run_status(
    cfg: &Config,
    cd_config: &CDConfig,
    vcs: &dyn Vcs,
    systemd: &dyn SystemdTrait,
    opts: &StatusOptions,
) -> i32 {
    let now_monotonic = monotonic_micros();
    let now_system = SystemTime::now();

    let report = collect(
        cfg,
        cd_config,
        vcs,
        systemd,
        opts,
        now_monotonic,
        now_system,
    );

    let written = if opts.json {
        render::write_json(&report, cfg)
    } else {
        render::write_plain(&report, cfg)
    };
    if let Err(e) = written {
        let _ = writeln!(cfg.output.err(), "[quadcd] status: failed to render: {e}");
        return 1;
    }

    report_exit_code(&report)
}

/// Pure collection step, parameterised on "now" so it is fully testable.
///
/// `now_monotonic_us` is microseconds since boot (same clock as the
/// `ActiveEnterTimestampMonotonic` property). `now_system` is wall-clock,
/// used only to convert the monotonic activation timestamp into a wall-clock
/// time so it can be compared against the unit file's mtime.
pub(crate) fn collect(
    cfg: &Config,
    cd_config: &CDConfig,
    vcs: &dyn Vcs,
    systemd: &dyn SystemdTrait,
    opts: &StatusOptions,
    now_monotonic_us: u64,
    now_system: SystemTime,
) -> StatusReport {
    let mode = if cfg.is_user_mode {
        Mode::User
    } else {
        Mode::System
    };

    let mut repo_names: Vec<&String> = cd_config.repositories.keys().collect();
    repo_names.sort();

    let mut repos = Vec::with_capacity(repo_names.len());
    let mut services = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    // Maps unit name → first repo that claimed it, so a duplicate can name
    // both sides in its warning.
    let mut seen_units: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for name in repo_names {
        let repo_cfg = &cd_config.repositories[name];
        let repo_dir = match safe_repo_dir(&cfg.data_dir, name) {
            Ok(p) => p,
            Err(e) => {
                repos.push(RepoStatus {
                    name: name.clone(),
                    url: repo_cfg.url.clone(),
                    branch: repo_cfg.branch.clone().unwrap_or_default(),
                    local_path: cfg.data_dir.join(name),
                    state: RepoState::Error { message: e },
                    fetched: false,
                    head_sha: None,
                });
                continue;
            }
        };

        let repo_status = collect_repo_status(name, repo_cfg, &repo_dir, vcs, opts);

        if let RepoState::Ahead { commits } = repo_status.state {
            warnings.push(format!(
                "repo '{name}' has {commits} local commit(s) not on origin"
            ));
        }

        // Enumerate units whenever the working tree exists. Any state other
        // than `Missing` implies `.git` was present when `collect_repo_status`
        // checked, so the `.git` re-check covers both error states (URL
        // mismatch, fetch failure, rev-list failure) and clean states.
        if repo_dir.join(".git").exists() {
            for filename in all_unit_files(&repo_dir) {
                let unit = unit_name_for_restart(&filename);
                if let Some(prev) = seen_units.get(&unit) {
                    warnings.push(format!(
                        "unit '{unit}' is defined in both '{prev}' and '{name}'; only the copy from '{prev}' is shown"
                    ));
                    continue;
                }
                seen_units.insert(unit.clone(), name.clone());
                let source_file = repo_dir.join(&filename);
                let state = systemd.show_state(&unit, cfg);
                let enabled = systemd.is_enabled(&unit, cfg);
                services.push(build_service_status(
                    unit,
                    name.clone(),
                    source_file,
                    state,
                    enabled,
                    now_monotonic_us,
                    now_system,
                ));
            }
        }

        repos.push(repo_status);
    }

    StatusReport {
        mode,
        config_path: cfg.config_path.clone(),
        repos,
        services,
        warnings,
    }
}

fn collect_repo_status(
    name: &str,
    repo_cfg: &crate::cd_config::RepoConfig,
    repo_dir: &std::path::Path,
    vcs: &dyn Vcs,
    opts: &StatusOptions,
) -> RepoStatus {
    let local_path = repo_dir.to_path_buf();
    let mut branch = repo_cfg.branch.clone().unwrap_or_default();

    if !repo_dir.join(".git").exists() {
        return RepoStatus {
            name: name.to_string(),
            url: repo_cfg.url.clone(),
            branch,
            local_path,
            state: RepoState::Missing,
            fetched: false,
            head_sha: None,
        };
    }

    let head_sha = vcs.head_sha(repo_dir);

    let actual_url = match vcs.remote_url(repo_dir) {
        Ok(u) => u,
        Err(e) => {
            return RepoStatus {
                name: name.to_string(),
                url: repo_cfg.url.clone(),
                branch,
                local_path,
                state: RepoState::Error { message: e },
                fetched: false,
                head_sha,
            };
        }
    };

    if actual_url != repo_cfg.url {
        return RepoStatus {
            name: name.to_string(),
            url: repo_cfg.url.clone(),
            branch,
            local_path,
            state: RepoState::UrlMismatch {
                configured: repo_cfg.url.clone(),
                actual: actual_url,
            },
            fetched: false,
            head_sha,
        };
    }

    if branch.is_empty() {
        branch = vcs.default_branch(repo_dir);
    }

    let mut fetched = false;
    if !opts.no_fetch {
        if let Err(e) = vcs.fetch(repo_dir) {
            return RepoStatus {
                name: name.to_string(),
                url: repo_cfg.url.clone(),
                branch,
                local_path,
                state: RepoState::Error {
                    message: format!("fetch failed: {e}"),
                },
                fetched: false,
                head_sha,
            };
        }
        fetched = true;
    }

    let remote_ref = format!("origin/{branch}");
    let state = match vcs.rev_list_left_right(repo_dir, "HEAD", &remote_ref) {
        Ok((0, 0)) => RepoState::UpToDate,
        Ok((ahead, 0)) => RepoState::Ahead { commits: ahead },
        Ok((0, behind)) => RepoState::Behind { commits: behind },
        Ok((ahead, behind)) => RepoState::Diverged { ahead, behind },
        Err(e) => RepoState::Error { message: e },
    };

    RepoStatus {
        name: name.to_string(),
        url: repo_cfg.url.clone(),
        branch,
        local_path,
        state,
        fetched,
        head_sha,
    }
}

fn build_service_status(
    unit: String,
    origin_repo: String,
    source_file: PathBuf,
    state: UnitState,
    enabled: String,
    now_monotonic_us: u64,
    now_system: SystemTime,
) -> ServiceStatus {
    let uptime = state.active_enter_timestamp_monotonic.and_then(|active| {
        now_monotonic_us
            .checked_sub(active)
            .map(Duration::from_micros)
    });

    // `restart_pending` = unit was reloaded (NeedDaemonReload=no) but the
    // running instance predates the on-disk unit definition. We compare the
    // FragmentPath mtime against the wall-clock equivalent of the monotonic
    // activation timestamp.
    let restart_pending = !state.need_daemon_reload
        && state.active_state == "active"
        && fragment_mtime_newer_than_active(state.fragment_path.as_deref(), uptime, now_system);

    let restart_loop_suspected = state.active_state == "active"
        && state.n_restarts >= RESTART_LOOP_MIN_RESTARTS
        && uptime.is_some_and(|u| u < RESTART_LOOP_MAX_UPTIME);

    ServiceStatus {
        unit,
        origin_repo,
        source_file,
        active_state: state.active_state,
        sub_state: state.sub_state,
        result: state.result,
        enabled,
        needs_daemon_reload: state.need_daemon_reload,
        restart_pending,
        n_restarts: state.n_restarts,
        uptime,
        restart_loop_suspected,
    }
}

fn fragment_mtime_newer_than_active(
    fragment_path: Option<&str>,
    uptime: Option<Duration>,
    now_system: SystemTime,
) -> bool {
    let (Some(path), Some(uptime)) = (fragment_path, uptime) else {
        return false;
    };
    let mtime = match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let active_wall = match now_system.checked_sub(uptime) {
        Some(t) => t,
        None => return false,
    };
    // mtime > active_wall ⇒ unit file changed after the service was last
    // started. Use UNIX_EPOCH-relative comparison to avoid Instant/SystemTime
    // ordering pitfalls when the wall clock has moved backwards.
    let mtime_us = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let active_us = active_wall
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    mtime_us > active_us
}

fn monotonic_micros() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Safety: passing a valid clock ID and a writable timespec.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64).saturating_mul(1_000_000) + (ts.tv_nsec as u64) / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cd_config::{CDConfig, RepoConfig};
    use crate::config::test_config;
    use crate::sync::testing::{MockSystemd, MockVcs};
    use std::collections::HashMap;

    fn cfg_with_data_dir(dir: PathBuf) -> Config {
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.data_dir = dir;
        cfg
    }

    fn cd_config_with(name: &str, url: &str, branch: Option<&str>) -> CDConfig {
        let mut map = HashMap::new();
        map.insert(
            name.to_string(),
            RepoConfig {
                url: url.to_string(),
                branch: branch.map(str::to_string),
                interval: None,
            },
        );
        CDConfig { repositories: map }
    }

    #[test]
    fn missing_repo_reports_missing_state() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));
        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            1_000_000,
            SystemTime::now(),
        );
        assert_eq!(report.repos.len(), 1);
        assert_eq!(report.repos[0].state, RepoState::Missing);
    }

    #[test]
    fn url_mismatch_state_detected_without_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        std::fs::create_dir_all(tmp.path().join("app").join(".git")).unwrap();
        let cd = cd_config_with("app", "https://configured/app.git", Some("main"));
        let vcs = MockVcs::new();
        *vcs.remote_url_val.borrow_mut() = Ok("https://other/app.git".to_string());
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(&cfg, &cd, &vcs, &systemd, &opts, 0, SystemTime::now());
        assert!(matches!(
            report.repos[0].state,
            RepoState::UrlMismatch { .. }
        ));
        assert!(!report.repos[0].fetched);
    }

    #[test]
    fn behind_state_from_rev_list() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        std::fs::create_dir_all(tmp.path().join("app").join(".git")).unwrap();
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));
        let vcs = MockVcs::new();
        *vcs.rev_list_default.borrow_mut() = Ok((0, 3));
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: false,
            json: false,
        };
        let report = collect(&cfg, &cd, &vcs, &systemd, &opts, 0, SystemTime::now());
        assert_eq!(report.repos[0].state, RepoState::Behind { commits: 3 });
        assert!(report.repos[0].fetched);
        assert!(*vcs.fetch_called.borrow());
    }

    #[test]
    fn no_fetch_skips_fetch_call() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        std::fs::create_dir_all(tmp.path().join("app").join(".git")).unwrap();
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));
        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let _ = collect(&cfg, &cd, &vcs, &systemd, &opts, 0, SystemTime::now());
        assert!(!*vcs.fetch_called.borrow());
    }

    #[test]
    fn service_status_picks_up_unit_files_from_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("app");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::write(repo_dir.join("web.container"), "[Container]\n").unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd
            .enabled_map
            .borrow_mut()
            .insert("web.service".to_string(), "enabled".to_string());
        systemd.set_active("web.service");
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            1_000_000,
            SystemTime::now(),
        );

        assert_eq!(report.services.len(), 1);
        let svc = &report.services[0];
        assert_eq!(svc.unit, "web.service");
        assert_eq!(svc.origin_repo, "app");
        assert_eq!(svc.enabled, "enabled");
        assert_eq!(svc.active_state, "active");
    }

    #[test]
    fn restart_loop_suspected_when_high_restarts_and_low_uptime() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("app");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::write(repo_dir.join("flap.container"), "x").unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.state_map.borrow_mut().insert(
            "flap.service".to_string(),
            UnitState {
                active_state: "active".to_string(),
                sub_state: "running".to_string(),
                result: "success".to_string(),
                need_daemon_reload: false,
                n_restarts: 7,
                // active 5 seconds ago (5_000_000 us before "now" of 10_000_000)
                active_enter_timestamp_monotonic: Some(5_000_000),
                fragment_path: None,
            },
        );
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            10_000_000,
            SystemTime::now(),
        );
        let svc = report
            .services
            .iter()
            .find(|s| s.unit == "flap.service")
            .unwrap();
        assert!(svc.restart_loop_suspected);
        assert_eq!(svc.n_restarts, 7);
        assert!(svc.uptime.is_some());
    }

    #[test]
    fn needs_daemon_reload_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("app");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::write(repo_dir.join("web.container"), "x").unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.state_map.borrow_mut().insert(
            "web.service".to_string(),
            UnitState {
                active_state: "active".to_string(),
                sub_state: "running".to_string(),
                result: "success".to_string(),
                need_daemon_reload: true,
                n_restarts: 0,
                active_enter_timestamp_monotonic: Some(1_000_000),
                fragment_path: None,
            },
        );
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            2_000_000,
            SystemTime::now(),
        );
        let svc = &report.services[0];
        assert!(svc.needs_daemon_reload);
        assert!(svc.is_problem());
    }

    #[test]
    fn restart_pending_when_fragment_newer_than_active_enter() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("app");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::write(repo_dir.join("web.container"), "x").unwrap();
        // The "loaded" fragment file is a separate path; we touch it now so its
        // mtime is recent. Active enter is far in the past, so mtime > active.
        let fragment = tmp.path().join("web.service.loaded");
        std::fs::write(&fragment, "x").unwrap();

        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        systemd.state_map.borrow_mut().insert(
            "web.service".to_string(),
            UnitState {
                active_state: "active".to_string(),
                sub_state: "running".to_string(),
                result: "success".to_string(),
                need_daemon_reload: false,
                n_restarts: 0,
                // active 1 hour ago in monotonic terms
                active_enter_timestamp_monotonic: Some(1_000_000),
                fragment_path: Some(fragment.to_string_lossy().to_string()),
            },
        );
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let now_monotonic = 1_000_000 + 3_600_000_000; // +1 hour
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            now_monotonic,
            SystemTime::now(),
        );
        let svc = &report.services[0];
        assert!(svc.restart_pending, "expected restart_pending: {svc:?}");
    }

    fn repo_status_with_state(state: RepoState) -> RepoStatus {
        RepoStatus {
            name: "x".into(),
            url: "u".into(),
            branch: "main".into(),
            local_path: PathBuf::from("/x"),
            state,
            fetched: true,
            head_sha: None,
        }
    }

    #[test]
    fn report_exit_code_zero_when_clean() {
        let report = StatusReport {
            mode: Mode::User,
            config_path: None,
            repos: Vec::new(),
            services: Vec::new(),
            warnings: Vec::new(),
        };
        assert_eq!(report_exit_code(&report), 0);
    }

    #[test]
    fn report_exit_code_nonzero_on_behind_repo() {
        let report = StatusReport {
            mode: Mode::User,
            config_path: None,
            repos: vec![repo_status_with_state(RepoState::Behind { commits: 1 })],
            services: Vec::new(),
            warnings: Vec::new(),
        };
        assert_eq!(report_exit_code(&report), 1);
    }

    #[test]
    fn ahead_state_is_not_a_problem() {
        let report = StatusReport {
            mode: Mode::User,
            config_path: None,
            repos: vec![repo_status_with_state(RepoState::Ahead { commits: 2 })],
            services: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(!report.repos[0].state.is_problem());
        assert_eq!(report_exit_code(&report), 0);
    }

    #[test]
    fn ahead_repo_emits_warning_but_no_problem_via_collect() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        std::fs::create_dir_all(tmp.path().join("app").join(".git")).unwrap();
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));
        let vcs = MockVcs::new();
        *vcs.rev_list_default.borrow_mut() = Ok((4, 0));
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(&cfg, &cd, &vcs, &systemd, &opts, 0, SystemTime::now());
        assert_eq!(report.repos[0].state, RepoState::Ahead { commits: 4 });
        assert_eq!(report_exit_code(&report), 0);
        assert!(
            report.warnings.iter().any(|w| w.contains("4 local commit")),
            "warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn diverged_state_from_rev_list() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());
        std::fs::create_dir_all(tmp.path().join("app").join(".git")).unwrap();
        let cd = cd_config_with("app", "https://example.com/repo.git", Some("main"));
        let vcs = MockVcs::new();
        *vcs.rev_list_default.borrow_mut() = Ok((2, 5));
        let systemd = MockSystemd::new();
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(&cfg, &cd, &vcs, &systemd, &opts, 0, SystemTime::now());
        assert_eq!(
            report.repos[0].state,
            RepoState::Diverged {
                ahead: 2,
                behind: 5
            }
        );
        assert_eq!(report_exit_code(&report), 1);
    }

    #[test]
    fn duplicate_unit_across_repos_emits_warning_and_keeps_first() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_data_dir(tmp.path().to_path_buf());

        for repo_name in ["alpha", "beta"] {
            let dir = tmp.path().join(repo_name);
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(dir.join("dup.container"), "x").unwrap();
        }

        let mut cd = CDConfig {
            repositories: HashMap::new(),
        };
        for name in ["alpha", "beta"] {
            cd.repositories.insert(
                name.to_string(),
                RepoConfig {
                    // MockVcs's default `remote_url` returns this exact URL,
                    // so both repos report up-to-date instead of url-mismatch.
                    url: "https://example.com/repo.git".to_string(),
                    branch: Some("main".to_string()),
                    interval: None,
                },
            );
        }

        let vcs = MockVcs::new();
        let systemd = MockSystemd::new();
        // Running, so the exit code reflects the duplicate warning alone
        // rather than a service that happens to be down.
        systemd.set_active("dup.service");
        let opts = StatusOptions {
            no_fetch: true,
            json: false,
        };
        let report = collect(
            &cfg,
            &cd,
            &vcs,
            &systemd,
            &opts,
            1_000_000,
            SystemTime::now(),
        );

        assert_eq!(report.services.len(), 1);
        assert_eq!(report.services[0].origin_repo, "alpha");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("dup.service") && w.contains("alpha") && w.contains("beta")),
            "warnings: {:?}",
            report.warnings
        );
        assert_eq!(report_exit_code(&report), 0);
    }
}
