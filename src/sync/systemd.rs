//! Systemd trait and implementation backed by the `systemctl` binary.

use std::io::Write;

use subprocess::{Exec, Redirection};

use crate::config::Config;

use super::cmd::run_with_markers;

/// Abstraction over systemctl operations.
///
/// `Systemd` shells out to systemctl; tests can substitute a mock that records
/// calls without requiring a running systemd.
pub trait SystemdTrait {
    fn daemon_reload(&self, cfg: &Config);
    fn restart(&self, units: &[String], cfg: &Config);
    fn start(&self, units: &[String], cfg: &Config);
    /// Return the `is-enabled` state string for a unit (e.g. "enabled", "static",
    /// "disabled", "masked", "generated"). Returns "unknown" on error.
    fn is_enabled(&self, unit: &str, cfg: &Config) -> String;
    /// Return `true` if the unit is currently active (running).
    fn is_active(&self, unit: &str, cfg: &Config) -> bool;
    /// List loaded unit names matching a glob pattern (e.g. "foo@*.service").
    fn list_units_matching(&self, pattern: &str, cfg: &Config) -> Vec<String>;
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
        pub enabled_map: RefCell<HashMap<String, String>>,
        pub active_set: RefCell<HashSet<String>>,
        pub listed_units: RefCell<HashMap<String, Vec<String>>>,
    }

    impl MockSystemd {
        pub fn new() -> Self {
            Self {
                reload_called: RefCell::new(false),
                restarted: RefCell::new(Vec::new()),
                started: RefCell::new(Vec::new()),
                enabled_map: RefCell::new(HashMap::new()),
                active_set: RefCell::new(HashSet::new()),
                listed_units: RefCell::new(HashMap::new()),
            }
        }
    }

    impl SystemdTrait for MockSystemd {
        fn daemon_reload(&self, _cfg: &Config) {
            *self.reload_called.borrow_mut() = true;
        }
        fn restart(&self, units: &[String], _cfg: &Config) {
            self.restarted.borrow_mut().extend_from_slice(units);
        }
        fn start(&self, units: &[String], _cfg: &Config) {
            self.started.borrow_mut().extend_from_slice(units);
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
        fn list_units_matching(&self, pattern: &str, _cfg: &Config) -> Vec<String> {
            self.listed_units
                .borrow()
                .get(pattern)
                .cloned()
                .unwrap_or_default()
        }
    }
}
