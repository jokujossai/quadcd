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
    /// `ActiveEnterTimestampMonotonic` (microseconds since boot). `None` when
    /// the unit has never become active or systemctl returned no value.
    /// Monotonic is used because it is emitted by `systemctl show` as a plain
    /// integer (the wall-clock variant is a localised date string).
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

    /// Treat any state other than `active` and `activating` as a failure
    /// worth surfacing to the operator after a start/restart.
    pub fn is_failure(&self) -> bool {
        !matches!(self.active_state.as_str(), "active" | "activating")
    }
}

/// `systemctl show` properties naming the units that, when active, would cause
/// systemd to start this unit.
///
/// These are the reverse (`…By`/`…Of`) dependency properties systemd derives
/// automatically from the forward setting on the *other* unit; they cannot be
/// written directly. Every property here has the same meaning for sync — "an
/// active unit over there implies this unit belongs running" — which is what
/// lets [`parse_reverse_deps`] union them into one flat list.
///
/// **Included**
/// - `WantedBy` (inverse of `Wants=`) and `RequiredBy` (inverse of
///   `Requires=`): the `[Install]` relationships, materialised as
///   `.wants/`/`.requires/` symlinks. Starting the dependant pulls this unit
///   in; at boot that dependant is typically `default.target`.
/// - `BoundBy` (inverse of `BindsTo=`): `BindsTo=` is `Requires=` plus
///   propagated stop, so an active binder starts this unit exactly as a
///   requirer would (and would stop again without it).
/// - `UpheldBy` (inverse of `Upholds=`): "as long as this unit is up, all
///   units listed in `Upholds=` are started whenever found to be inactive or
///   failed". An active upholder is the strongest possible statement that
///   this unit should be running — systemd would restart it continuously.
///
/// **Deliberately excluded**
/// - `ConsistsOf` (inverse of `PartOf=`): `PartOf=` configures dependencies
///   "similar to `Requires=`, but limited to stopping and restarting of
///   units". It never propagates a *start*, so an active `PartOf=` parent
///   says nothing about whether this unit should run — including it would
///   start units a reboot would leave stopped, which is exactly the state
///   divergence sync is trying to avoid.
/// - `RequisiteOf` (inverse of `Requisite=`): `Requisite=` deliberately does
///   not start the unit; it fails the *dependant* if this unit is not
///   already active. So an active requisite-of unit is evidence this unit was
///   already up, not a reason to start it.
/// - `TriggeredBy` (`.socket`/`.timer`/`.path` units): the entire point of
///   socket, timer and path activation is that the service starts on demand.
///   An active `.socket` means the service is *ready to be* started, and boot
///   leaves it inactive — starting it during sync would diverge from the
///   state a reboot produces. See the note in `plan_activation` about the
///   pre-pull side of this trade-off.
///
///   This does not make sync ignore triggered services: `plan_activation`
///   tests `is_active` first and short-circuits, so a socket-activated
///   service that happens to be running when its file changes is restarted
///   like any other active unit. This property set only ever gates the
///   *inactive* branch.
/// - `ConflictedBy` (inverse of `Conflicts=`): a negative relationship. An
///   active conflicting unit forces this one *stopped*.
/// - `StopPropagatedFrom`/`ReloadPropagatedFrom` (inverses of
///   `PropagatesStopTo=`/`PropagatesReloadTo=`): propagate stop and reload
///   respectively, never start.
/// - `Before`/`After`: pure ordering. They constrain *when* a unit starts
///   relative to another, never *whether* it starts at all.
///
/// The names must match systemd's spelling exactly. `systemctl show` fetches
/// the unit's properties over D-Bus and filters the reply against the names
/// asked for, so a name the running systemd does not know simply matches
/// nothing: no line is emitted for it and the exit status is still 0. A typo
/// would therefore degrade silently to "no reverse dependencies" rather than
/// failing loudly. The names are verified against `systemd.unit(5)` and the
/// `org.freedesktop.systemd1` `Unit` interface.
///
/// The same mechanism makes an old systemd degrade gracefully: `UpheldBy`
/// arrived in systemd 249, and below that it contributes nothing instead of
/// erroring.
const START_AUTHORISING_PROPERTIES: [&str; 4] = ["WantedBy", "RequiredBy", "BoundBy", "UpheldBy"];

/// Parse the unit names out of a `systemctl show --value` listing of
/// [`START_AUTHORISING_PROPERTIES`].
///
/// Splitting the whole output on whitespace unions the properties, which is
/// only sound because every queried property authorises a start identically —
/// none of them needs to be told apart from the others. `--value` prints bare
/// values in systemd's own property order, one line per property, with nothing
/// to say which line is which: a property that is *known but empty* emits a
/// blank line, and a property the running systemd does not implement (such as
/// `UpheldBy` before systemd 249) emits no line at all, silently shifting
/// every later line up. Positions are therefore unusable, and a property
/// needing different treatment would have to be fetched without `--value` and
/// parsed as `KEY=value` lines instead. The union is immune to both cases.
///
/// Unit names never contain whitespace, so the split is unambiguous. Results
/// are deduplicated: a unit that both wants and requires this one is listed by
/// two properties but is a single reverse dependency.
fn parse_reverse_deps(stdout: &str) -> Vec<String> {
    let mut deps: Vec<String> = Vec::new();
    for name in stdout.split_whitespace() {
        if !deps.iter().any(|d| d == name) {
            deps.push(name.to_string());
        }
    }
    deps
}

/// Abstraction over systemctl operations.
///
/// `Systemd` shells out to systemctl; tests can substitute a mock that records
/// calls without requiring a running systemd.
pub trait SystemdTrait {
    fn daemon_reload(&self, cfg: &Config);
    fn restart(&self, units: &[String], cfg: &Config);
    fn start(&self, units: &[String], cfg: &Config);
    fn stop(&self, units: &[String], cfg: &Config);
    /// Return the `is-enabled` state string for a unit (e.g. "enabled", "static",
    /// "disabled", "masked", "generated"). Returns "unknown" on error.
    fn is_enabled(&self, unit: &str, cfg: &Config) -> String;
    /// Return `true` if the unit is currently active (running).
    fn is_active(&self, unit: &str, cfg: &Config) -> bool;
    /// Return the units whose activation would make systemd start this unit:
    /// the reverse dependencies `WantedBy`, `RequiredBy`, `BoundBy` and
    /// `UpheldBy` (see `START_AUTHORISING_PROPERTIES` for why those four and
    /// no others), including targets linked via generator
    /// `.wants`/`.requires` symlinks. Deduplicated; empty on error.
    fn reverse_deps(&self, unit: &str, cfg: &Config) -> Vec<String>;
    /// List loaded unit names matching a glob pattern (e.g. "foo@*.service").
    ///
    /// Backed by `list-units --all`, so inactive and failed units are
    /// reported alongside running ones: callers that care about the state —
    /// activation does, stopping does not — must check it themselves.
    fn list_units_matching(&self, pattern: &str, cfg: &Config) -> Vec<String>;
    /// Return the unit's `ActiveState`, `SubState`, and `Result` via
    /// `systemctl show`. Returns `UnitState::unknown()` if the call fails.
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

    fn is_active(&self, unit: &str, cfg: &Config) -> bool {
        let mut args = Self::user_args(cfg);
        args.extend(["is-active", "--quiet", unit]);

        self.exec()
            .args(args.iter().copied())
            .capture()
            .is_ok_and(|c| c.success())
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
        let mut args = Self::user_args(cfg);
        args.extend([
            "show",
            unit,
            "--property=ActiveState",
            "--property=SubState",
            "--property=Result",
            "--property=NeedDaemonReload",
            "--property=NRestarts",
            "--property=ActiveEnterTimestampMonotonic",
            "--property=FragmentPath",
        ]);

        let Ok(capture) = self.exec().args(args.iter().copied()).capture() else {
            return UnitState::unknown();
        };
        if !capture.success() {
            return UnitState::unknown();
        }

        let stdout = String::from_utf8_lossy(&capture.stdout);
        let mut state = UnitState::unknown();
        for line in stdout.lines() {
            if let Some((key, val)) = line.split_once('=') {
                match key {
                    "ActiveState" => state.active_state = val.to_string(),
                    "SubState" => state.sub_state = val.to_string(),
                    "Result" => state.result = val.to_string(),
                    "NeedDaemonReload" => state.need_daemon_reload = val == "yes",
                    "NRestarts" => state.n_restarts = val.parse().unwrap_or(0),
                    "ActiveEnterTimestampMonotonic" => {
                        state.active_enter_timestamp_monotonic =
                            val.parse::<u64>().ok().filter(|v| *v > 0);
                    }

                    "FragmentPath" => {
                        state.fragment_path = if val.is_empty() {
                            None
                        } else {
                            Some(val.to_string())
                        };
                    }
                    _ => {}
                }
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
        // `systemctl show` filters the D-Bus reply against the requested
        // names, so a property this systemd does not know emits *no* line
        // rather than a blank one. On systemd < 249 `UpheldBy` does not exist,
        // so only three lines come back — the union does not care that the
        // remaining lines shifted up.
        assert_eq!(
            parse_reverse_deps("default.target\n\n\n"),
            vec!["default.target".to_string()]
        );
        // A hypothetical systemd knowing none of the four (or a typo in every
        // name) yields no output at all, which is the intended fail-closed
        // "no reverse dependencies".
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
    use std::collections::{HashMap, HashSet};

    pub struct MockSystemd {
        pub reload_called: RefCell<bool>,
        pub restarted: RefCell<Vec<String>>,
        pub started: RefCell<Vec<String>>,
        pub stopped: RefCell<Vec<String>>,
        /// Records the order of trait method invocations (`"reload"`, `"stop:foo"`,
        /// `"restart:bar"`, …) so tests can assert ordering across methods.
        pub call_log: RefCell<Vec<String>>,
        pub enabled_map: RefCell<HashMap<String, String>>,
        pub active_set: RefCell<HashSet<String>>,
        /// Canned [`SystemdTrait::reverse_deps`] answers. Flat lists, like the
        /// real implementation: every start-authorising relationship it
        /// queries authorises a start identically, so there is nothing for a
        /// test to tell apart here — a `WantedBy` entry and an `UpheldBy`
        /// entry are indistinguishable by construction.
        pub reverse_deps_map: RefCell<HashMap<String, Vec<String>>>,
        pub listed_units: RefCell<HashMap<String, Vec<String>>>,
        pub state_map: RefCell<HashMap<String, UnitState>>,
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
                active_set: RefCell::new(HashSet::new()),
                reverse_deps_map: RefCell::new(HashMap::new()),
                listed_units: RefCell::new(HashMap::new()),
                state_map: RefCell::new(HashMap::new()),
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
            }
        }
        fn start(&self, units: &[String], _cfg: &Config) {
            self.started.borrow_mut().extend_from_slice(units);
            for u in units {
                self.call_log.borrow_mut().push(format!("start:{u}"));
            }
        }
        fn stop(&self, units: &[String], _cfg: &Config) {
            self.stopped.borrow_mut().extend_from_slice(units);
            for u in units {
                self.call_log.borrow_mut().push(format!("stop:{u}"));
            }
        }
        fn is_enabled(&self, unit: &str, _cfg: &Config) -> String {
            self.enabled_map
                .borrow()
                .get(unit)
                .cloned()
                .unwrap_or_else(|| "disabled".to_string())
        }
        fn is_active(&self, unit: &str, _cfg: &Config) -> bool {
            self.active_set.borrow().contains(unit)
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
            self.state_map
                .borrow()
                .get(unit)
                .cloned()
                .unwrap_or_else(|| UnitState {
                    active_state: "active".to_string(),
                    sub_state: "running".to_string(),
                    result: "success".to_string(),
                    need_daemon_reload: false,
                    n_restarts: 0,
                    active_enter_timestamp_monotonic: None,
                    fragment_path: None,
                })
        }
    }
}
