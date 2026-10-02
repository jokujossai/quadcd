//! Systemd trait and implementation backed by the `systemctl` binary.

use std::io::Write;

use subprocess::{Exec, Redirection};

use crate::config::Config;

use super::cmd::run_with_markers;

/// Snapshot of a unit's runtime state, derived from `systemctl show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitState {
    pub active_state: String,
    pub sub_state: String,
    pub result: String,
    /// `systemctl show NeedDaemonReload=yes` — unit file on disk has changed
    /// since systemd last loaded it.
    pub need_daemon_reload: bool,
    /// `NRestarts` — total restart count for the current invocation.
    pub n_restarts: u32,
    /// `ActiveEnterTimestampMonotonic` (µs since boot); monotonic because the
    /// wall-clock variant is a localised string.
    pub active_enter_timestamp_monotonic: Option<u64>,
    /// `FragmentPath` — path to the unit file currently loaded by systemd.
    pub fragment_path: Option<String>,
}

impl UnitState {
    pub fn unknown() -> Self {
        Self {
            active_state: "unknown".to_string(),
            sub_state: "unknown".to_string(),
            result: "unknown".to_string(),
            need_daemon_reload: false,
            n_restarts: 0,
            active_enter_timestamp_monotonic: None,
            fragment_path: None,
        }
    }

    /// Anything but `active`/`activating` after a start or restart.
    pub fn is_failure(&self) -> bool {
        !matches!(self.active_state.as_str(), "active" | "activating")
    }
}

/// A unit's `ActiveState` and `SubState`, from one `systemctl show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationState {
    pub active_state: String,
    pub sub_state: String,
}

impl ActivationState {
    pub fn new(active_state: &str, sub_state: &str) -> Self {
        Self {
            active_state: active_state.to_string(),
            sub_state: sub_state.to_string(),
        }
    }

    /// Every predicate below answers `false` for this.
    pub fn unknown() -> Self {
        Self::new("unknown", "unknown")
    }

    /// Running, by `systemctl is-active`'s rule (includes `reloading`, `refreshing`).
    pub fn is_active(&self) -> bool {
        matches!(
            self.active_state.as_str(),
            "active" | "reloading" | "refreshing"
        )
    }

    /// `ActiveState=activating`.
    pub fn is_starting(&self) -> bool {
        self.active_state == "activating"
    }

    /// `activating (auto-restart)`: failed and waiting out `Restart=`; nothing
    /// is actually starting.
    pub fn is_auto_restarting(&self) -> bool {
        self.is_starting() && self.sub_state == "auto-restart"
    }
}

/// Reverse dependencies whose active unit would make systemd start this one.
///
/// Excluded because they never start a unit: `ConsistsOf` (`PartOf=`),
/// `RequisiteOf`, `TriggeredBy` (socket/timer/path start on demand),
/// `ConflictedBy`, `*PropagatedFrom`, ordering. Unknown names (a typo, or
/// `UpheldBy` before systemd 249) silently match nothing.
const START_AUTHORISING_PROPERTIES: [&str; 4] = ["WantedBy", "RequiredBy", "BoundBy", "UpheldBy"];

/// Deduplicated unit names from `systemctl show --value` output. Lines cannot
/// be told apart (missing properties shift them), so all are unioned; that is
/// fine because every property means the same.
fn parse_reverse_deps(stdout: &str) -> Vec<String> {
    let mut deps: Vec<String> = Vec::new();
    for name in stdout.split_whitespace() {
        if !deps.iter().any(|d| d == name) {
            deps.push(name.to_string());
        }
    }
    deps
}

/// systemctl operations; mocked in tests.
pub trait SystemdTrait {
    fn daemon_reload(&self, cfg: &Config);
    fn restart(&self, units: &[String], cfg: &Config);
    fn start(&self, units: &[String], cfg: &Config);
    fn stop(&self, units: &[String], cfg: &Config);
    /// `is-enabled` output (e.g. "enabled", "generated"); "unknown" on error.
    fn is_enabled(&self, unit: &str, cfg: &Config) -> String;
    /// Running, by `systemctl is-active`'s rule. Defaults to [`SystemdTrait::activation_state`].
    fn is_active(&self, unit: &str, cfg: &Config) -> bool {
        self.activation_state(unit, cfg).is_active()
    }
    /// `ActiveState` and `SubState`. Defaults to [`SystemdTrait::show_state`];
    /// [`Systemd`] queries only these two properties.
    fn activation_state(&self, unit: &str, cfg: &Config) -> ActivationState {
        let state = self.show_state(unit, cfg);
        ActivationState {
            active_state: state.active_state,
            sub_state: state.sub_state,
        }
    }
    /// Units with a queued start or restart job (from `list-jobs`, which has
    /// the job type). Such units still read `inactive`. Empty on error.
    /// Includes `shutdown.target` during a reboot; systemd refuses those
    /// starts, so the cost is a wasted pull and a logged failure.
    fn pending_start_jobs(&self, cfg: &Config) -> Vec<String>;
    /// Units that would start this one (`START_AUTHORISING_PROPERTIES`).
    /// Deduplicated; empty on error.
    fn reverse_deps(&self, unit: &str, cfg: &Config) -> Vec<String>;
    /// Loaded units matching a glob (`foo@*.service`), in any state.
    fn list_units_matching(&self, pattern: &str, cfg: &Config) -> Vec<String>;
    /// Full [`UnitState`] via `systemctl show`; `UnitState::unknown()` on error.
    fn show_state(&self, unit: &str, cfg: &Config) -> UnitState;
}

/// Systemctl implementation backed by the `systemctl` binary.
pub struct Systemd {
    cmd: String,
    env: Vec<(String, String)>,
}

impl Default for Systemd {
    fn default() -> Self {
        Self::new()
    }
}

impl Systemd {
    /// Create a `Systemd` using the default `systemctl` binary.
    pub fn new() -> Self {
        Self {
            cmd: "systemctl".to_string(),
            env: Vec::new(),
        }
    }

    /// Create a `Systemd` with a custom command path.
    pub fn with_command(cmd: &str) -> Self {
        Self {
            cmd: cmd.to_string(),
            env: Vec::new(),
        }
    }

    /// Add an environment variable to all spawned commands.
    pub fn with_env(mut self, key: &str, val: &str) -> Self {
        self.env.push((key.to_string(), val.to_string()));
        self
    }

    fn exec(&self) -> Exec {
        let mut e = Exec::cmd(&self.cmd).stdin(Redirection::Null);
        for (k, v) in &self.env {
            e = e.env(k, v);
        }
        e
    }

    /// Build the common args prefix: optional `--user` flag.
    fn user_args(cfg: &Config) -> Vec<&'static str> {
        if cfg.is_user_mode {
            vec!["--user"]
        } else {
            vec![]
        }
    }

    /// `systemctl show --property=...` as `KEY=value` pairs; `None` on failure.
    fn show_properties(
        &self,
        unit: &str,
        properties: &[&str],
        cfg: &Config,
    ) -> Option<Vec<(String, String)>> {
        let mut args: Vec<String> = Self::user_args(cfg)
            .into_iter()
            .map(str::to_string)
            .collect();
        args.push("show".to_string());
        args.push(unit.to_string());
        args.extend(properties.iter().map(|p| format!("--property={p}")));

        let capture = self.exec().args(&args).capture().ok()?;
        if !capture.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&capture.stdout)
                .lines()
                .filter_map(|line| {
                    line.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect(),
        )
    }
}

impl SystemdTrait for Systemd {
    fn daemon_reload(&self, cfg: &Config) {
        let mut args = Self::user_args(cfg);
        args.push("daemon-reload");

        if cfg.verbose {
            let mode = if cfg.is_user_mode { "--user " } else { "" };
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Running systemctl {mode}daemon-reload"
            );
        }

        let label = format!("{} {}", self.cmd, args.join(" "));
        match run_with_markers(
            self.exec().args(args.iter().copied()),
            &label,
            cfg.subprocess_output.as_ref(),
        ) {
            Ok(s) if !s.success() => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] systemctl daemon-reload exited with {s}"
                );
            }
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Failed to run systemctl daemon-reload: {e}"
                );
            }
            _ => {}
        }
    }

    fn restart(&self, units: &[String], cfg: &Config) {
        let mut args = Self::user_args(cfg);
        args.push("restart");
        let unit_refs: Vec<&str> = units.iter().map(|s| s.as_str()).collect();
        args.extend(&unit_refs);

        let unit_list = units.join(" ");
        let label = format!("{} {}", self.cmd, args.join(" "));
        match run_with_markers(
            self.exec().args(args.iter().copied()),
            &label,
            cfg.subprocess_output.as_ref(),
        ) {
            Ok(s) if !s.success() => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] restart {unit_list} exited with {s}"
                );
            }
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Failed to restart {unit_list}: {e}"
                );
            }
            Ok(_) => {
                if cfg.verbose {
                    let _ = writeln!(cfg.output.err(), "[quadcd] Restarted {unit_list}");
                }
            }
        }
    }

    fn start(&self, units: &[String], cfg: &Config) {
        let mut args = Self::user_args(cfg);
        args.push("start");
        let unit_refs: Vec<&str> = units.iter().map(|s| s.as_str()).collect();
        args.extend(&unit_refs);

        let unit_list = units.join(" ");
        let label = format!("{} {}", self.cmd, args.join(" "));
        match run_with_markers(
            self.exec().args(args.iter().copied()),
            &label,
            cfg.subprocess_output.as_ref(),
        ) {
            Ok(s) if !s.success() => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] start {unit_list} exited with {s}"
                );
            }
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Failed to start {unit_list}: {e}"
                );
            }
            Ok(_) => {
                if cfg.verbose {
                    let _ = writeln!(cfg.output.err(), "[quadcd] Started {unit_list}");
                }
            }
        }
    }

    fn stop(&self, units: &[String], cfg: &Config) {
        let mut args = Self::user_args(cfg);
        args.push("stop");
        let unit_refs: Vec<&str> = units.iter().map(|s| s.as_str()).collect();
        args.extend(&unit_refs);

        let unit_list = units.join(" ");
        let label = format!("{} {}", self.cmd, args.join(" "));
        match run_with_markers(
            self.exec().args(args.iter().copied()),
            &label,
            cfg.subprocess_output.as_ref(),
        ) {
            Ok(s) if !s.success() => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] stop {unit_list} exited with {s}"
                );
            }
            Err(e) => {
                let _ = writeln!(cfg.output.err(), "[quadcd] Failed to stop {unit_list}: {e}");
            }
            Ok(_) => {
                if cfg.verbose {
                    let _ = writeln!(cfg.output.err(), "[quadcd] Stopped {unit_list}");
                }
            }
        }
    }

    fn is_enabled(&self, unit: &str, cfg: &Config) -> String {
        let mut args = Self::user_args(cfg);
        args.extend(["is-enabled", unit]);

        match self.exec().args(args.iter().copied()).capture() {
            Ok(capture) => String::from_utf8_lossy(&capture.stdout).trim().to_string(),
            Err(_) => "unknown".to_string(),
        }
    }

    fn activation_state(&self, unit: &str, cfg: &Config) -> ActivationState {
        // Cheaper than show_state: NeedDaemonReload makes PID 1 stat files.
        let Some(props) = self.show_properties(unit, &["ActiveState", "SubState"], cfg) else {
            return ActivationState::unknown();
        };
        let mut state = ActivationState::unknown();
        for (key, val) in props {
            match key.as_str() {
                "ActiveState" => state.active_state = val,
                "SubState" => state.sub_state = val,
                _ => {}
            }
        }
        state
    }

    fn pending_start_jobs(&self, cfg: &Config) -> Vec<String> {
        let mut args = Self::user_args(cfg);
        args.extend(["list-jobs", "--no-legend", "--no-pager"]);

        let Ok(capture) = self.exec().args(args.iter().copied()).capture() else {
            return Vec::new();
        };
        if !capture.success() {
            return Vec::new();
        }

        // `JOB UNIT TYPE STATE`; header and footer drop out on the type filter.
        String::from_utf8_lossy(&capture.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let (_job_id, unit, job_type) = (fields.next()?, fields.next()?, fields.next()?);
                // `stop` and `reload` jobs say nothing about a unit coming up.
                matches!(job_type, "start" | "restart").then(|| unit.to_string())
            })
            .collect()
    }

    fn reverse_deps(&self, unit: &str, cfg: &Config) -> Vec<String> {
        // Owned args here (unlike the other methods): the `--property=` flags
        // are built from START_AUTHORISING_PROPERTIES so the property list has
        // exactly one definition.
        let mut args: Vec<String> = Self::user_args(cfg)
            .into_iter()
            .map(str::to_string)
            .collect();
        args.push("show".to_string());
        args.push(unit.to_string());
        args.extend(
            START_AUTHORISING_PROPERTIES
                .iter()
                .map(|p| format!("--property={p}")),
        );
        args.push("--value".to_string());

        match self.exec().args(&args).capture() {
            Ok(capture) if capture.success() => {
                parse_reverse_deps(&String::from_utf8_lossy(&capture.stdout))
            }
            _ => Vec::new(),
        }
    }

    fn list_units_matching(&self, pattern: &str, cfg: &Config) -> Vec<String> {
        let mut args = Self::user_args(cfg);
        args.extend(["list-units", pattern, "--no-legend", "--plain", "--all"]);

        match self.exec().args(args.iter().copied()).capture() {
            Ok(capture) if capture.success() => String::from_utf8_lossy(&capture.stdout)
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .map(|s| s.to_string())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn show_state(&self, unit: &str, cfg: &Config) -> UnitState {
        let Some(props) = self.show_properties(
            unit,
            &[
                "ActiveState",
                "SubState",
                "Result",
                "NeedDaemonReload",
                "NRestarts",
                "ActiveEnterTimestampMonotonic",
                "FragmentPath",
            ],
            cfg,
        ) else {
            return UnitState::unknown();
        };
        let mut state = UnitState::unknown();
        for (key, val) in props {
            match key.as_str() {
                "ActiveState" => state.active_state = val,
                "SubState" => state.sub_state = val,
                "Result" => state.result = val,
                "NeedDaemonReload" => state.need_daemon_reload = val == "yes",
                "NRestarts" => state.n_restarts = val.parse().unwrap_or(0),
                "ActiveEnterTimestampMonotonic" => {
                    state.active_enter_timestamp_monotonic =
                        val.parse::<u64>().ok().filter(|v| *v > 0);
                }
                "FragmentPath" => {
                    state.fragment_path = if val.is_empty() { None } else { Some(val) };
                }
                _ => {}
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_deps_properties_exclude_non_starting_relationships() {
        // `PartOf=`/`Requisite=` never start a unit, socket/timer activation is
        // on demand, and `Conflicts=` stops it. See the const's documentation.
        for excluded in [
            "ConsistsOf",
            "RequisiteOf",
            "TriggeredBy",
            "ConflictedBy",
            "StopPropagatedFrom",
            "ReloadPropagatedFrom",
        ] {
            assert!(
                !START_AUTHORISING_PROPERTIES.contains(&excluded),
                "{excluded} must not authorise a start"
            );
        }
    }

    #[test]
    fn parse_reverse_deps_unions_all_properties() {
        // `systemctl show -p WantedBy -p RequiredBy -p BoundBy -p UpheldBy
        // --value` emits one line per property, in systemd's own order.
        let stdout = "default.target\nconsumer.service\nbinder.service\nsupervisor.service\n";
        assert_eq!(
            parse_reverse_deps(stdout),
            vec![
                "default.target".to_string(),
                "consumer.service".to_string(),
                "binder.service".to_string(),
                "supervisor.service".to_string(),
            ]
        );
    }

    #[test]
    fn parse_reverse_deps_handles_multiple_units_per_property() {
        // `WantedBy` names two targets on one line; `BoundBy` and `UpheldBy`
        // are known but empty and contribute the two trailing blank lines.
        let stdout = "multi-user.target default.target\nconsumer.service\n\n\n";
        assert_eq!(
            parse_reverse_deps(stdout),
            vec![
                "multi-user.target".to_string(),
                "default.target".to_string(),
                "consumer.service".to_string(),
            ]
        );
    }

    #[test]
    fn parse_reverse_deps_skips_empty_properties() {
        // Only `UpheldBy` is populated; the three properties that are known
        // but empty emit blank lines that must not become dependency names.
        assert_eq!(
            parse_reverse_deps("\n\n\nsupervisor.service\n"),
            vec!["supervisor.service".to_string()]
        );
        assert_eq!(parse_reverse_deps("\n\n\n\n"), Vec::<String>::new());
    }

    #[test]
    fn parse_reverse_deps_handles_properties_the_systemd_does_not_implement() {
        // systemd < 249: no UpheldBy line at all.
        assert_eq!(
            parse_reverse_deps("default.target\n\n\n"),
            vec!["default.target".to_string()]
        );
        assert_eq!(parse_reverse_deps(""), Vec::<String>::new());
    }

    #[test]
    fn parse_reverse_deps_deduplicates_across_properties() {
        // A unit that both wants and requires this one appears in two
        // properties but is a single reverse dependency.
        assert_eq!(
            parse_reverse_deps("app.target\napp.target\napp.target\n\n"),
            vec!["app.target".to_string()]
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::new_without_default)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    pub struct MockSystemd {
        pub reload_called: RefCell<bool>,
        pub restarted: RefCell<Vec<String>>,
        pub started: RefCell<Vec<String>>,
        pub stopped: RefCell<Vec<String>>,
        /// Records the order of trait method invocations (`"reload"`, `"stop:foo"`,
        /// `"restart:bar"`, …) so tests can assert ordering across methods.
        pub call_log: RefCell<Vec<String>>,
        pub enabled_map: RefCell<HashMap<String, String>>,
        /// Single source for every state query; absent = `inactive (dead)`.
        /// Populate with the `set_*` helpers.
        pub state_map: RefCell<HashMap<String, UnitState>>,
        /// Units with a queued start job (still `inactive`).
        pub queued_start_jobs: RefCell<Vec<String>>,
        /// Canned [`SystemdTrait::reverse_deps`] answers.
        pub reverse_deps_map: RefCell<HashMap<String, Vec<String>>>,
        pub listed_units: RefCell<HashMap<String, Vec<String>>>,
    }

    impl MockSystemd {
        pub fn new() -> Self {
            Self {
                reload_called: RefCell::new(false),
                restarted: RefCell::new(Vec::new()),
                started: RefCell::new(Vec::new()),
                stopped: RefCell::new(Vec::new()),
                call_log: RefCell::new(Vec::new()),
                enabled_map: RefCell::new(HashMap::new()),
                state_map: RefCell::new(HashMap::new()),
                queued_start_jobs: RefCell::new(Vec::new()),
                reverse_deps_map: RefCell::new(HashMap::new()),
                listed_units: RefCell::new(HashMap::new()),
            }
        }

        /// Set a unit's `ActiveState`/`SubState`, leaving the rest of its
        /// [`UnitState`] at the defaults.
        pub fn set_state(&self, unit: &str, active_state: &str, sub_state: &str) {
            let mut state = UnitState {
                active_state: active_state.to_string(),
                sub_state: sub_state.to_string(),
                result: "success".to_string(),
                need_daemon_reload: false,
                n_restarts: 0,
                active_enter_timestamp_monotonic: None,
                fragment_path: None,
            };
            if let Some(existing) = self.state_map.borrow().get(unit) {
                state.result = existing.result.clone();
                state.need_daemon_reload = existing.need_daemon_reload;
                state.n_restarts = existing.n_restarts;
                state.active_enter_timestamp_monotonic = existing.active_enter_timestamp_monotonic;
                state.fragment_path = existing.fragment_path.clone();
            }
            self.state_map.borrow_mut().insert(unit.to_string(), state);
        }

        /// `active (running)` — the unit is up.
        pub fn set_active(&self, unit: &str) {
            self.set_state(unit, "active", "running");
        }

        /// `activating (start)` — part-way through starting.
        pub fn set_activating(&self, unit: &str) {
            self.set_state(unit, "activating", "start");
        }

        /// `reloading (reload)` — running, reloading its configuration.
        /// `systemctl is-active` exits 0 here.
        pub fn set_reloading(&self, unit: &str) {
            self.set_state(unit, "reloading", "reload");
        }

        /// `activating (auto-restart)` — failed and waiting out `Restart=`.
        pub fn set_auto_restarting(&self, unit: &str) {
            self.set_state(unit, "activating", "auto-restart");
        }

        /// Queue a start job for a unit, as systemd would while the unit waits
        /// for whatever is ordered before it.
        pub fn queue_start_job(&self, unit: &str) {
            self.queued_start_jobs.borrow_mut().push(unit.to_string());
        }

        fn state_of(&self, unit: &str) -> UnitState {
            self.state_map
                .borrow()
                .get(unit)
                .cloned()
                .unwrap_or_else(|| UnitState {
                    active_state: "inactive".to_string(),
                    sub_state: "dead".to_string(),
                    result: "success".to_string(),
                    need_daemon_reload: false,
                    n_restarts: 0,
                    active_enter_timestamp_monotonic: None,
                    fragment_path: None,
                })
        }

        /// Mark a started unit active, unless the test pinned its state.
        fn record_activation(&self, unit: &str) {
            if !self.state_map.borrow().contains_key(unit) {
                self.set_active(unit);
            }
        }
    }

    impl SystemdTrait for MockSystemd {
        fn daemon_reload(&self, _cfg: &Config) {
            *self.reload_called.borrow_mut() = true;
            self.call_log.borrow_mut().push("reload".to_string());
        }
        fn restart(&self, units: &[String], _cfg: &Config) {
            self.restarted.borrow_mut().extend_from_slice(units);
            for u in units {
                self.call_log.borrow_mut().push(format!("restart:{u}"));
                self.record_activation(u);
            }
        }
        fn start(&self, units: &[String], _cfg: &Config) {
            self.started.borrow_mut().extend_from_slice(units);
            for u in units {
                self.call_log.borrow_mut().push(format!("start:{u}"));
                self.record_activation(u);
            }
        }
        fn stop(&self, units: &[String], _cfg: &Config) {
            self.stopped.borrow_mut().extend_from_slice(units);
            for u in units {
                self.call_log.borrow_mut().push(format!("stop:{u}"));
                self.set_state(u, "inactive", "dead");
            }
        }
        fn is_enabled(&self, unit: &str, _cfg: &Config) -> String {
            self.enabled_map
                .borrow()
                .get(unit)
                .cloned()
                .unwrap_or_else(|| "disabled".to_string())
        }
        fn activation_state(&self, unit: &str, _cfg: &Config) -> ActivationState {
            // Logged so tests can count queries: each of these is one
            // `systemctl show` against PID 1 in production.
            self.call_log.borrow_mut().push(format!("state:{unit}"));
            let state = self.state_of(unit);
            ActivationState::new(&state.active_state, &state.sub_state)
        }
        fn pending_start_jobs(&self, _cfg: &Config) -> Vec<String> {
            self.call_log.borrow_mut().push("list-jobs".to_string());
            self.queued_start_jobs.borrow().clone()
        }
        fn reverse_deps(&self, unit: &str, _cfg: &Config) -> Vec<String> {
            self.reverse_deps_map
                .borrow()
                .get(unit)
                .cloned()
                .unwrap_or_default()
        }
        fn list_units_matching(&self, pattern: &str, _cfg: &Config) -> Vec<String> {
            self.listed_units
                .borrow()
                .get(pattern)
                .cloned()
                .unwrap_or_default()
        }
        fn show_state(&self, unit: &str, _cfg: &Config) -> UnitState {
            self.state_of(unit)
        }
    }
}
