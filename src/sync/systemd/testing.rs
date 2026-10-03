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
