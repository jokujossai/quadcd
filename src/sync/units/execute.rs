use std::io::Write;

#[cfg(test)]
use std::collections::HashSet;

use crate::config::Config;

use super::super::SystemdTrait;
use super::files::{is_template_unit, unit_name_for_restart};
use super::plan::{ActivationPlan, ActiveStates};

/// Carry out an [`ActivationPlan`]; returns the units that failed to come up.
pub(crate) fn execute_activation(
    systemd: &dyn SystemdTrait,
    plan: &ActivationPlan,
    cfg: &Config,
) -> Vec<String> {
    let ActivationPlan {
        to_start,
        to_restart,
        start_on_sync,
        notes,
        ..
    } = plan;

    if cfg.verbose {
        for note in notes {
            let _ = writeln!(cfg.output.err(), "[quadcd] {note}");
        }
    }

    // StartOnSync= units first, in their own blocking call, so a build
    // finishes before anything using its image starts.
    let (first_start, rest_start): (Vec<String>, Vec<String>) = to_start
        .iter()
        .cloned()
        .partition(|u| start_on_sync.contains(u));
    let (first_restart, rest_restart): (Vec<String>, Vec<String>) = to_restart
        .iter()
        .cloned()
        .partition(|u| start_on_sync.contains(u));
    run_batch(
        systemd,
        &first_start,
        &first_restart,
        " (StartOnSync, first)",
        cfg,
    );
    run_batch(systemd, &rest_start, &rest_restart, "", cfg);

    let mut activated: Vec<String> = to_start.iter().chain(to_restart.iter()).cloned().collect();
    activated.sort();
    activated.dedup();

    let mut failed: Vec<String> = Vec::new();
    for unit in &activated {
        let state = systemd.show_state(unit, cfg);
        let _ = writeln!(
            cfg.output.err(),
            "[quadcd] {unit}: {} ({})",
            state.active_state,
            state.sub_state
        );
        if state.is_failure() {
            failed.push(unit.clone());
        }
    }

    if !failed.is_empty() {
        let _ = writeln!(
            cfg.output.err(),
            "[quadcd] {} service(s) failed after restart: {}",
            failed.len(),
            failed.join(", ")
        );
    }

    failed
}

/// Issue one `start` and one `restart` call for a batch of planned units.
fn run_batch(
    systemd: &dyn SystemdTrait,
    to_start: &[String],
    to_restart: &[String],
    label: &str,
    cfg: &Config,
) {
    if cfg.verbose {
        if !to_start.is_empty() {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Starting units{label}: {}",
                to_start.join(", ")
            );
        }
        if !to_restart.is_empty() {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Restarting units{label}: {}",
                to_restart.join(", ")
            );
        }
    }
    if !to_start.is_empty() {
        systemd.start(to_start, cfg);
    }
    if !to_restart.is_empty() {
        systemd.restart(to_restart, cfg);
    }
}

/// Plan and execute in one step, for tests.
#[cfg(test)]
pub(crate) fn activate_changed_units_inner(
    systemd: &dyn SystemdTrait,
    changed_files: &[String],
    cfg: &Config,
) -> Vec<String> {
    let plan = super::plan::plan_activation(systemd, changed_files, &HashSet::new(), cfg);
    execute_activation(systemd, &plan, cfg)
}

/// Stop units whose files were deleted. Must run before `daemon-reload`, or
/// their containers are orphaned.
///
/// Templates stop every loaded instance; other units are stopped if running or
/// activating. A unit with only a queued start job is left alone: there is no
/// container yet, and stopping it would propagate to a requiring boot target.
pub(crate) fn stop_deleted_units_inner(
    systemd: &dyn SystemdTrait,
    deleted_files: &[String],
    cfg: &Config,
) {
    let mut units: Vec<String> = deleted_files
        .iter()
        .map(|f| unit_name_for_restart(f))
        .collect();
    units.sort();
    units.dedup();

    if units.is_empty() {
        return;
    }

    let mut to_stop: Vec<String> = Vec::new();
    let mut active = ActiveStates::new(systemd);

    for unit in &units {
        if is_template_unit(unit) {
            let pattern = unit.replace("@.", "@*.");
            let instances = systemd.list_units_matching(&pattern, cfg);
            if cfg.verbose {
                if instances.is_empty() {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Template {unit}: no loaded instances to stop"
                    );
                } else {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Template {unit}: stopping instances: {}",
                        instances.join(", ")
                    );
                }
            }
            to_stop.extend(instances);
            continue;
        }

        if active.is_running_or_starting(unit, cfg) {
            to_stop.push(unit.clone());
        } else if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Not stopping deleted {unit} (not running)"
            );
        }
    }

    if !to_stop.is_empty() {
        if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Stopping deleted units: {}",
                to_stop.join(", ")
            );
        }
        systemd.stop(&to_stop, cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;

    use super::super::super::systemd::testing::MockSystemd;
    use super::super::super::systemd::UnitState;
    use super::super::plan::plan_activation;

    fn start_on_sync(units: &[&str]) -> HashSet<String> {
        units.iter().map(|u| u.to_string()).collect()
    }

    // activate_changed_units_inner

    #[test]
    fn restart_deduplicates_units() {
        let systemd = MockSystemd::new();
        for unit in &["app.service", "app-volume.service", "web.service"] {
            systemd.set_active(unit);
        }
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        let changed = vec![
            "app.container".to_string(),
            "app.volume".to_string(),
            "web.service".to_string(),
            "web.service".to_string(),
        ];

        activate_changed_units_inner(&systemd, &changed, &cfg);

        let restarted = systemd.restarted.borrow();
        assert_eq!(restarted.len(), 3);
        assert!(restarted.contains(&"app.service".to_string()));
        assert!(restarted.contains(&"app-volume.service".to_string()));
        assert!(restarted.contains(&"web.service".to_string()));
    }

    #[test]
    fn restart_empty_list_does_nothing() {
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &[], &cfg);
        assert!(systemd.restarted.borrow().is_empty());
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn execute_activation_runs_planned_units() {
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("web.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["app.container".into(), "web.service".into()],
            &HashSet::new(),
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
        assert_eq!(systemd.restarted.borrow().as_slice(), &["web.service"]);
    }

    #[test]
    fn execute_activation_runs_start_on_sync_units_first() {
        // First deploy: the container is wanted by default.target and the
        // build is not wanted by anything. The build must be started, and
        // finish, before the container — in its own call, because nothing
        // orders the two.
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("web.service");
        systemd.set_active("web-build.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &[
                "app.build".into(),
                "app.container".into(),
                "web.build".into(),
                "web.container".into(),
            ],
            &start_on_sync(&["app-build.service", "web-build.service"]),
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        let actions: Vec<String> = systemd
            .call_log
            .borrow()
            .iter()
            .filter(|c| c.starts_with("start:") || c.starts_with("restart:"))
            .cloned()
            .collect();
        assert_eq!(
            actions,
            &[
                "start:app-build.service",
                "restart:web-build.service",
                "start:app.service",
                "restart:web.service",
            ]
        );
    }

    #[test]
    fn activate_verbose_logs_actions() {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "new.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");
        systemd.set_active("running.service");

        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        let changed = vec![
            "new.service".to_string(),
            "running.service".to_string(),
            "skip.service".to_string(),
        ];
        activate_changed_units_inner(&systemd, &changed, &cfg);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("Starting units"),
            "expected starting log in: {stderr}"
        );
        assert!(
            stderr.contains("Restarting units"),
            "expected restarting log in: {stderr}"
        );
        assert!(
            stderr.contains("Skipping inactive skip.service"),
            "expected skip log in: {stderr}"
        );
    }

    #[test]
    fn activate_reports_active_state_per_unit() {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");
        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));

        let failed = activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);
        assert!(failed.is_empty(), "no failure expected, got {failed:?}");

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("app.service: active (running)"),
            "expected per-unit state log in: {stderr}"
        );
        assert!(
            !stderr.contains("service(s) failed after restart"),
            "no aggregated failure line should be emitted when all units are active: {stderr}"
        );
    }

    #[test]
    fn activate_reports_failed_state_and_returns_failures() {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");
        systemd.state_map.borrow_mut().insert(
            "app.service".to_string(),
            UnitState {
                active_state: "failed".to_string(),
                sub_state: "failed".to_string(),
                result: "exit-code".to_string(),
                need_daemon_reload: false,
                n_restarts: 0,
                active_enter_timestamp_monotonic: None,
                fragment_path: None,
            },
        );
        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));

        let failed = activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);
        assert_eq!(failed, vec!["app.service".to_string()]);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("app.service: failed (failed)"),
            "expected failure state log in: {stderr}"
        );
        assert!(
            stderr.contains("1 service(s) failed after restart: app.service"),
            "expected failure summary in: {stderr}"
        );
    }

    #[test]
    fn activate_summary_lists_multiple_failures() {
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        for unit in &["a.service", "b.service"] {
            systemd
                .reverse_deps_map
                .borrow_mut()
                .insert(unit.to_string(), vec!["default.target".to_string()]);
            systemd.state_map.borrow_mut().insert(
                unit.to_string(),
                UnitState {
                    active_state: "failed".to_string(),
                    sub_state: "failed".to_string(),
                    result: "exit-code".to_string(),
                    need_daemon_reload: false,
                    n_restarts: 0,
                    active_enter_timestamp_monotonic: None,
                    fragment_path: None,
                },
            );
        }
        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));

        let failed =
            activate_changed_units_inner(&systemd, &["a.service".into(), "b.service".into()], &cfg);
        assert_eq!(failed.len(), 2);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("2 service(s) failed after restart: a.service, b.service"),
            "expected aggregated failure summary in: {stderr}"
        );
    }

    #[test]
    fn restart_verbose_logs_units() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        let changed = vec!["app.container".to_string()];
        activate_changed_units_inner(&systemd, &changed, &cfg);

        let stderr = err_buf.captured();
        assert!(stderr.contains("Restarting units"));
        assert!(stderr.contains("app.service"));
    }

    // stop_deleted_units_inner

    #[test]
    fn stop_deleted_units_inner_stops_active_units() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        systemd.set_active("data-volume.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(
            &systemd,
            &["app.container".to_string(), "data.volume".to_string()],
            &cfg,
        );

        let stopped = systemd.stopped.borrow();
        assert_eq!(stopped.len(), 2);
        assert!(stopped.contains(&"app.service".to_string()));
        assert!(stopped.contains(&"data-volume.service".to_string()));
    }

    #[test]
    fn stop_deleted_units_inner_skips_inactive() {
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["gone.container".to_string()], &cfg);

        assert!(systemd.stopped.borrow().is_empty());
    }

    #[test]
    fn stop_deleted_units_inner_stops_activating_unit() {
        // A unit caught mid-start would finish bringing its container up just
        // before `daemon-reload` removes the unit file, orphaning it.
        let systemd = MockSystemd::new();
        systemd.set_activating("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["app.container".to_string()], &cfg);

        assert_eq!(systemd.stopped.borrow().as_slice(), &["app.service"]);
    }

    #[test]
    fn stop_deleted_units_inner_stops_reloading_unit() {
        // `systemctl is-active` exits 0 for `reloading`: the unit is running,
        // its container is up, and it has to be stopped like any other.
        let systemd = MockSystemd::new();
        systemd.set_reloading("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["app.container".to_string()], &cfg);

        assert_eq!(systemd.stopped.borrow().as_slice(), &["app.service"]);
    }

    #[test]
    fn stop_deleted_units_inner_leaves_unit_with_only_a_queued_job_alone() {
        // The job has not run, so there is no container to orphan — and
        // stopping it here, before `daemon-reload`, would cancel a boot
        // transaction's start job and propagate the stop over the live
        // `.requires/` edge to the requiring target.
        let systemd = MockSystemd::new();
        systemd.queue_start_job("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["app.container".to_string()], &cfg);

        assert!(systemd.stopped.borrow().is_empty());
    }

    #[test]
    fn stop_deleted_units_inner_leaves_stopped_unit_alone() {
        // Inactive with nothing behind it: nothing to orphan.
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["app.container".to_string()], &cfg);

        assert!(systemd.stopped.borrow().is_empty());
    }

    #[test]
    fn stop_deleted_units_inner_empty_does_nothing() {
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &[], &cfg);

        assert!(systemd.stopped.borrow().is_empty());
    }

    #[test]
    fn stop_deleted_units_inner_template_stops_all_instances() {
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec![
                "myapp@web.service".to_string(),
                "myapp@worker.service".to_string(),
            ],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        stop_deleted_units_inner(&systemd, &["myapp@.container".to_string()], &cfg);

        let stopped = systemd.stopped.borrow();
        assert!(stopped.contains(&"myapp@web.service".to_string()));
        assert!(stopped.contains(&"myapp@worker.service".to_string()));
    }

    #[test]
    fn stop_deleted_units_inner_deduplicates() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        // Same generated unit shows up twice (e.g. .container and .service
        // entries that both map to app.service).
        stop_deleted_units_inner(
            &systemd,
            &[
                "app.container".to_string(),
                "app.service".to_string(),
                "app.container".to_string(),
            ],
            &cfg,
        );

        let stopped = systemd.stopped.borrow();
        assert_eq!(stopped.len(), 1);
        assert!(stopped.contains(&"app.service".to_string()));
    }

    #[test]
    fn stop_deleted_units_inner_verbose_logs() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        stop_deleted_units_inner(
            &systemd,
            &["app.container".to_string(), "skip.container".to_string()],
            &cfg,
        );

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("Stopping deleted units"),
            "expected stop log in: {stderr}"
        );
        assert!(
            stderr.contains("Not stopping deleted skip.service"),
            "expected skip log in: {stderr}"
        );
    }
}
