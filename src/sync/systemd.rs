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

/// A unit's `ActiveState` and `SubState` — the one pair every activation
/// decision is derived from.
///
/// Both come from a single `systemctl show`, so a caller asking several
/// questions about the same unit ("is it running?", "is it on its way up?")
/// pays for one query and cannot get answers that contradict each other.
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

    /// The state systemctl reports when it cannot say — every predicate below
    /// answers `false`, which is the conservative reading for all of them.
    pub fn unknown() -> Self {
        Self::new("unknown", "unknown")
    }

    /// Running, by the same rule as `systemctl is-active`.
    ///
    /// That command exits 0 for `reloading` as well as `active`, and since
    /// systemd v254 also for `refreshing` (a soft-reboot mount refresh). A
    /// unit reloading its configuration is running throughout — dropping
    /// either state here would silently narrow behaviour against the
    /// `is-active` call this replaced.
    pub fn is_active(&self) -> bool {
        matches!(
            self.active_state.as_str(),
            "active" | "reloading" | "refreshing"
        )
    }

    /// Part-way through starting: `ActiveState=activating`.
    pub fn is_starting(&self) -> bool {
        self.active_state == "activating"
    }

    /// Waiting out `Restart=` between attempts: `activating (auto-restart)`.
    ///
    /// Reported as `activating`, but nothing is starting — the unit failed and
    /// systemd is holding it in a restart backoff. A crash-looping unit can sit
    /// here indefinitely, so it is not evidence that anything it wants belongs
    /// running.
    pub fn is_auto_restarting(&self) -> bool {
        self.is_starting() && self.sub_state == "auto-restart"
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
    /// Return `true` if the unit is currently active (running), by
    /// `systemctl is-active`'s rule — which also covers `reloading` and, since
    /// systemd v254, `refreshing`.
    ///
    /// Defaults to projecting [`SystemdTrait::activation_state`], the same
    /// backward-compat pattern that method uses for [`SystemdTrait::show_state`].
    fn is_active(&self, unit: &str, cfg: &Config) -> bool {
        self.activation_state(unit, cfg).is_active()
    }
    /// Return the unit's `ActiveState` and `SubState`.
    ///
    /// Every activation decision is derived from this pair, so one query
    /// answers all of them for a unit. `systemctl is-active` cannot stand in:
    /// it collapses `activating` and `failed` into the same non-zero exit, and
    /// says nothing about the sub-state that separates a unit genuinely
    /// starting from one idling in `auto-restart` backoff.
    ///
    /// Defaults to projecting [`SystemdTrait::show_state`], so implementors
    /// need not add anything; [`Systemd`] overrides it with a narrower
    /// `systemctl show` that asks for just these two properties.
    fn activation_state(&self, unit: &str, cfg: &Config) -> ActivationState {
        let state = self.show_state(unit, cfg);
        ActivationState {
            active_state: state.active_state,
            sub_state: state.sub_state,
        }
    }
    /// Return the units that have a queued start (or restart) job.
    ///
    /// A unit whose start job is queued but has not run yet is still
    /// `ActiveState=inactive`, `SubState=dead` — indistinguishable from a
    /// stopped unit by state alone. This is how a target looks while the units
    /// ordered before it are starting: it implicitly orders itself after
    /// everything it wants, so its own job cannot complete until theirs have.
    ///
    /// Read from `systemctl list-jobs` rather than per-unit
    /// `show --property=Job` because that property carries only the job id.
    /// The type is what matters: a queued `stop` job means the opposite of a
    /// queued `start`, and only `list-jobs` reports it.
    ///
    /// Job *types* are filtered, not job semantics: a reboot enqueues a
    /// **start** job for `shutdown.target`, and this returns it like any
    /// other. The residual cost is small — systemd refuses a start it
    /// considers destructive for `DefaultDependencies=yes` units, so a unit
    /// wanted by a shutdown target yields a wasted image pull and a failed
    /// `systemctl start` in the log, not a running unit. Empty on error.
    fn pending_start_jobs(&self, cfg: &Config) -> Vec<String>;
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

    /// Run `systemctl show <unit> --property=<p>...` for `properties` and
    /// parse the `KEY=value` lines it prints. `None` on a failed or
    /// non-zero-exit invocation — shared by [`SystemdTrait::activation_state`]
    /// and [`SystemdTrait::show_state`], which differ only in which
    /// properties they ask for and which struct they fold the pairs into.
    ///
    /// Parsed as `KEY=value` lines rather than with `--value`: the bare
    /// values arrive in systemd's own property order with nothing to say
    /// which line is which.
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
        // Overrides the trait's `show_state`-based default with a two-property
        // query. `show_state` also asks for `NeedDaemonReload`, which makes PID
        // 1 stat the fragment and rescan the drop-in directories on every read
        // — work worth doing for the status report it exists for, but not for
        // the two strings the planner needs per unit.
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

        // Columns are `JOB UNIT TYPE STATE`, e.g.
        //   175 default.target      start waiting
        //   176 quadcd-sync.service start running
        // Selecting on the type column also makes the parse independent of
        // `--no-legend` actually suppressing the decorations: the header row's
        // type column reads `TYPE` and the `N jobs listed.` footer has too few
        // columns, so both drop out on their own.
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
        /// Every unit's state, as the one `ActiveState`/`SubState` pair systemd
        /// would report. `is_active`, `activation_state` and `show_state` all
        /// read this, so the mock cannot express a combination systemd cannot
        /// produce — a unit that is `is_active() == false` while `show_state()`
        /// says `active`, say, which would hide any mismatch between the
        /// predicates and `systemctl is-active`.
        ///
        /// Units absent from the map are `inactive (dead)`; the `set_*` helpers
        /// below are the intended way to populate it.
        pub state_map: RefCell<HashMap<String, UnitState>>,
        /// Units with a queued start job: still `inactive`, but systemd is on
        /// its way to bringing them up. This is how a boot target looks while
        /// the units ordered before it are starting.
        pub queued_start_jobs: RefCell<Vec<String>>,
        /// Canned [`SystemdTrait::reverse_deps`] answers. Flat lists, like the
        /// real implementation: every start-authorising relationship it
        /// queries authorises a start identically, so there is nothing for a
        /// test to tell apart here — a `WantedBy` entry and an `UpheldBy`
        /// entry are indistinguishable by construction.
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

        /// Bring a unit up the way a successful `systemctl start`/`restart`
        /// does, so post-activation state reads reflect the actions taken.
        /// A state a test pinned explicitly wins — that is how a unit that
        /// fails to come up is expressed.
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
