use std::io::Write;
use std::path::Path;

use crate::config::Config;
use crate::install::{find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};

use super::SystemdTrait;

/// List all unit files in a repo directory.
///
/// This uses the same recursive discovery rules as install mode so sync sees
/// nested units and ignores hidden directories such as `.git`.
pub(crate) fn all_unit_files(repo_dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = find_files(repo_dir, QUADLET_EXTENSIONS)
        .into_iter()
        .chain(find_files(repo_dir, SYSTEMD_EXTENSIONS))
        .filter_map(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Check whether a filename has a recognised unit-file extension.
pub(crate) fn is_unit_file(name: &str) -> bool {
    let ext = match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return false,
    };
    QUADLET_EXTENSIONS.contains(&ext) || SYSTEMD_EXTENSIONS.contains(&ext)
}

/// Map a unit filename to the systemd unit name to restart.
pub(crate) fn unit_name_for_restart(filename: &str) -> String {
    // For Quadlet files, derive the generated systemd unit name.
    if let Some(unit) = crate::install::generated_unit_name(filename) {
        return unit;
    }
    // Plain systemd units: strip leading path components, keep just the filename.
    Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename)
        .to_string()
}

/// Return `true` if `unit_name` is a systemd template (e.g. `foo@.service`).
pub(crate) fn is_template_unit(unit_name: &str) -> bool {
    Path::new(unit_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.ends_with('@'))
}

/// What sync intends to do with a set of changed units.
///
/// Produced by [`plan_activation`] and carried out by [`execute_activation`].
/// The decision is split from the execution so callers can act on it before
/// anything is started — sync uses it to pre-pull images only for the units it
/// is actually going to activate.
#[derive(Debug, Default)]
pub(crate) struct ActivationPlan {
    /// Inactive units to `systemctl start`.
    to_start: Vec<String>,
    /// Active units — and running instances of changed templates — to
    /// `systemctl restart`.
    to_restart: Vec<String>,
    /// Unit names as derived from the changed files, restricted to the ones
    /// that will end up running: those started or restarted directly, plus
    /// those systemd pulls in as a dependency of a unit being started.
    /// Templates appear un-instantiated (`foo@.service`) so a changed file
    /// always maps back to a single entry.
    activating: Vec<String>,
    /// Verbose lines describing the decisions, emitted by
    /// [`execute_activation`] rather than at planning time so a plan can be
    /// computed without logging (sync plans twice around a slow image pull).
    notes: Vec<String>,
}

impl ActivationPlan {
    /// Return `true` if the unit backing `filename` will be running after
    /// this plan is executed — started, restarted, or pulled in by systemd
    /// as a dependency of a unit being started.
    pub(crate) fn activates_file(&self, filename: &str) -> bool {
        let unit = unit_name_for_restart(filename);
        self.activating.contains(&unit)
    }
}

/// Decide how each changed unit should be activated.
///
/// Must run **after** `daemon-reload` so the systemd queries below see the
/// updated unit files. Each changed unit is inspected:
/// - **Templates** (`foo@.service`): discover running instances via
///   `list-units` and restart them.
/// - **Active** units: `restart` — a running unit whose file changed keeps
///   running with the new configuration.
/// - **Inactive** units: `start` only if some unit that wants or requires
///   them is active. This mirrors boot: quadcd-deployed units are all
///   `generated`, so the only thing that starts them at boot is a
///   `.wants`/`.requires` link from an active unit (typically
///   `default.target`, materialised from `[Install]`). Anything else —
///   including units an operator stopped by hand — is left alone, so sync
///   never creates a running state that a reboot would not reproduce.
///
/// Planning performs no systemd state changes and logs nothing; the verbose
/// account of the decisions is stored on the plan and written out by
/// [`execute_activation`].
pub(crate) fn plan_activation(
    systemd: &dyn SystemdTrait,
    changed_files: &[String],
    cfg: &Config,
) -> ActivationPlan {
    let mut units: Vec<String> = changed_files
        .iter()
        .map(|f| unit_name_for_restart(f))
        .collect();
    units.sort();
    units.dedup();

    let mut plan = ActivationPlan::default();
    // Units left alone, with the reverse dependencies that failed to justify
    // starting them. Revisited below: a dependant this plan starts will drag
    // them in even though sync does not name them itself.
    let mut skipped: Vec<(String, Vec<String>)> = Vec::new();

    for unit in &units {
        if is_template_unit(unit) {
            // Discover running instances for this template
            let pattern = unit.replace("@.", "@*.");
            let instances = systemd.list_units_matching(&pattern, cfg);
            if instances.is_empty() {
                plan.notes
                    .push(format!("Template {unit}: no running instances found"));
            } else {
                plan.notes.push(format!(
                    "Template {unit}: restarting instances: {}",
                    instances.join(", ")
                ));
                plan.activating.push(unit.clone());
            }
            plan.to_restart.extend(instances);
            continue;
        }

        if systemd.is_active(unit, cfg) {
            // Always restart a running unit whose file changed.
            plan.to_restart.push(unit.clone());
            plan.activating.push(unit.clone());
            continue;
        }

        // Inactive: start only if boot would — i.e. something that wants or
        // requires this unit is itself active. `[Install]` links show up
        // here as active targets (e.g. default.target); a unit nothing
        // depends on, or one whose dependants are stopped, stays stopped.
        let deps = systemd.reverse_deps(unit, cfg);
        if deps.iter().any(|dep| systemd.is_active(dep, cfg)) {
            plan.to_start.push(unit.clone());
            plan.activating.push(unit.clone());
        } else {
            skipped.push((unit.clone(), deps));
        }
    }

    mark_transitively_activated(&mut plan, &mut skipped);

    for (unit, deps) in &skipped {
        plan.notes.push(format!(
            "Skipping inactive {unit} (not wanted by any active unit; wanted/required by: [{}])",
            deps.join(", ")
        ));
    }

    plan
}

/// Move units that systemd will start as a side effect into the plan's
/// `activating` set.
///
/// A skipped unit is not started by sync, but if something sync *is* starting
/// wants or requires it, systemd brings it up in the same transaction — just
/// as boot would. `web.image` required by a `web.container` being started is
/// the common case: its image still has to be pre-pulled, or the download
/// lands inline during unit start. Iterates to a fixed point so chains
/// (`a` → `b` → `c`) are covered.
fn mark_transitively_activated(
    plan: &mut ActivationPlan,
    skipped: &mut Vec<(String, Vec<String>)>,
) {
    // Only `to_start` seeds this: units in `to_restart` are already active,
    // so their dependencies were resolved by the `is_active` check above.
    let mut pending: Vec<String> = plan.to_start.clone();

    while let Some(activated) = pending.pop() {
        let mut i = 0;
        while i < skipped.len() {
            if skipped[i].1.contains(&activated) {
                let (unit, _) = skipped.remove(i);
                plan.activating.push(unit.clone());
                pending.push(unit);
            } else {
                i += 1;
            }
        }
    }
}

/// Carry out an [`ActivationPlan`] and report which units failed to come up.
///
/// Returns the list of units whose post-activation `ActiveState` is anything
/// other than `active`/`activating` (i.e. failed to come up).
pub(crate) fn execute_activation(
    systemd: &dyn SystemdTrait,
    plan: &ActivationPlan,
    cfg: &Config,
) -> Vec<String> {
    let ActivationPlan {
        to_start,
        to_restart,
        notes,
        ..
    } = plan;

    if cfg.verbose {
        for note in notes {
            let _ = writeln!(cfg.output.err(), "[quadcd] {note}");
        }
        if !to_start.is_empty() {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Starting units: {}",
                to_start.join(", ")
            );
        }
        if !to_restart.is_empty() {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Restarting units: {}",
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

/// Plan and immediately execute activation for `changed_files`.
///
/// Convenience wrapper around [`plan_activation`] + [`execute_activation`] for
/// callers that do not need to inspect the plan in between.
#[cfg(test)]
pub(crate) fn activate_changed_units_inner(
    systemd: &dyn SystemdTrait,
    changed_files: &[String],
    cfg: &Config,
) -> Vec<String> {
    let plan = plan_activation(systemd, changed_files, cfg);
    execute_activation(systemd, &plan, cfg)
}

/// Stop units whose backing files were deleted from the repo.
///
/// Must be called **before** `daemon-reload`: once systemd no longer sees the
/// unit file, `systemctl stop` cannot reach the running container/process and
/// it becomes orphaned.
///
/// - **Templates** (`foo@.service`): every running instance is stopped.
/// - **Regular units**: stopped only if currently active.
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

    for unit in &units {
        if is_template_unit(unit) {
            let pattern = unit.replace("@.", "@*.");
            let instances = systemd.list_units_matching(&pattern, cfg);
            if cfg.verbose {
                if instances.is_empty() {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Template {unit}: no running instances to stop"
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

        if systemd.is_active(unit, cfg) {
            to_stop.push(unit.clone());
        } else if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Not stopping deleted {unit} (not active)"
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
    use rstest::rstest;
    use std::fs;

    use super::super::systemd::testing::MockSystemd;

    // is_unit_file

    #[test]
    fn is_unit_file_quadlet_extensions() {
        assert!(is_unit_file("app.container"));
        assert!(is_unit_file("data.volume"));
        assert!(is_unit_file("net.network"));
        assert!(is_unit_file("k8s.kube"));
        assert!(is_unit_file("img.image"));
        assert!(is_unit_file("b.build"));
        assert!(is_unit_file("p.pod"));
        assert!(is_unit_file("a.artifact"));
    }

    #[test]
    fn is_unit_file_systemd_extensions() {
        assert!(is_unit_file("app.service"));
        assert!(is_unit_file("app.timer"));
        assert!(is_unit_file("app.socket"));
        assert!(is_unit_file("dev.device"));
        assert!(is_unit_file("mnt.mount"));
        assert!(is_unit_file("s.swap"));
        assert!(is_unit_file("t.target"));
        assert!(is_unit_file("p.path"));
        assert!(is_unit_file("s.slice"));
        assert!(is_unit_file("s.scope"));
        assert!(is_unit_file("a.automount"));
    }

    #[test]
    fn is_unit_file_unknown_extension() {
        assert!(!is_unit_file("readme.txt"));
        assert!(!is_unit_file("config.yaml"));
        assert!(!is_unit_file("noext"));
        assert!(!is_unit_file(".hidden"));
    }

    // unit_name_for_restart

    #[rstest]
    #[case::container("app.container", "app.service")]
    #[case::kube("k8s.kube", "k8s.service")]
    #[case::image("img.image", "img-image.service")]
    #[case::build("b.build", "b-build.service")]
    #[case::volume("data.volume", "data-volume.service")]
    #[case::network("net.network", "net-network.service")]
    #[case::service_passthrough("app.service", "app.service")]
    #[case::timer_passthrough("app.timer", "app.timer")]
    #[case::pod("p.pod", "p-pod.service")]
    #[case::artifact("a.artifact", "a-artifact.service")]
    #[case::strips_path_service("some/path/app.service", "app.service")]
    #[case::strips_path_volume("some/path/data.volume", "data-volume.service")]
    fn test_unit_name_for_restart(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(unit_name_for_restart(input), expected);
    }

    // all_unit_files

    #[test]
    fn all_unit_files_finds_units() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("app.container"), "").unwrap();
        fs::write(tmp.path().join("web.service"), "").unwrap();
        fs::write(tmp.path().join("readme.md"), "").unwrap();
        let files = all_unit_files(tmp.path());
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"app.container".to_string()));
        assert!(files.contains(&"web.service".to_string()));
    }

    #[test]
    fn all_unit_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(all_unit_files(tmp.path()).is_empty());
    }

    #[test]
    fn all_unit_files_recurses_and_skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("nested");
        let hidden = tmp.path().join(".git");
        fs::create_dir(&nested).unwrap();
        fs::create_dir(&hidden).unwrap();
        fs::write(nested.join("worker.timer"), "").unwrap();
        fs::write(nested.join("app.container"), "").unwrap();
        fs::write(hidden.join("ignored.service"), "").unwrap();
        fs::write(tmp.path().join("web.service"), "").unwrap();

        let files = all_unit_files(tmp.path());
        assert_eq!(
            files,
            vec![
                "app.container".to_string(),
                "web.service".to_string(),
                "worker.timer".to_string(),
            ]
        );
    }

    // is_template_unit

    #[test]
    fn is_template_unit_detects_template() {
        assert!(is_template_unit("foo@.service"));
        assert!(is_template_unit("bar@.service"));
    }

    #[test]
    fn is_template_unit_regular_unit() {
        assert!(!is_template_unit("foo.service"));
        assert!(!is_template_unit("foo@instance.service"));
    }

    // activate_changed_units_inner

    #[test]
    fn restart_deduplicates_units() {
        let systemd = MockSystemd::new();
        for unit in &["app.service", "app-volume.service", "web.service"] {
            systemd.active_set.borrow_mut().insert(unit.to_string());
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

    #[rstest]
    #[case::inactive_wanted_by_active_target(&["default.target"], &["default.target"], false, "start")]
    #[case::inactive_required_by_active_unit(&["consumer.service"], &["consumer.service"], false, "start")]
    #[case::inactive_wanted_by_inactive_unit(&["consumer.service"], &[], false, "skip")]
    #[case::inactive_one_of_many_deps_active(&["a.service", "b.target"], &["b.target"], false, "start")]
    #[case::inactive_without_deps_skipped(&[], &[], false, "skip")]
    #[case::active_without_deps_restarts(&[], &[], true, "restart")]
    #[case::active_with_inactive_deps_restarts(&["consumer.service"], &[], true, "restart")]
    fn activate_unit_by_state(
        #[case] reverse_deps: &[&str],
        #[case] active_deps: &[&str],
        #[case] is_active: bool,
        #[case] expected: &str,
    ) {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            reverse_deps.iter().map(|s| s.to_string()).collect(),
        );
        for dep in active_deps {
            systemd.active_set.borrow_mut().insert(dep.to_string());
        }
        if is_active {
            systemd
                .active_set
                .borrow_mut()
                .insert("app.service".to_string());
        }
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        match expected {
            "start" => {
                assert!(systemd
                    .started
                    .borrow()
                    .contains(&"app.service".to_string()));
                assert!(systemd.restarted.borrow().is_empty());
            }
            "restart" => {
                assert!(systemd
                    .restarted
                    .borrow()
                    .contains(&"app.service".to_string()));
                assert!(systemd.started.borrow().is_empty());
            }
            "skip" => {
                assert!(systemd.started.borrow().is_empty());
                assert!(systemd.restarted.borrow().is_empty());
            }
            _ => panic!("unknown expected action: {expected}"),
        }
    }

    // plan_activation / ActivationPlan::activates_file

    #[rstest]
    #[case::started_unit_is_activated(&["default.target"], &["default.target"], false, true)]
    #[case::restarted_unit_is_activated(&[], &[], true, true)]
    #[case::skipped_inactive_unit_is_not_activated(&["consumer.service"], &[], false, false)]
    #[case::unit_without_deps_is_not_activated(&[], &[], false, false)]
    fn plan_activates_file_matches_planned_action(
        #[case] reverse_deps: &[&str],
        #[case] active_deps: &[&str],
        #[case] is_active: bool,
        #[case] expected: bool,
    ) {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            reverse_deps.iter().map(|s| s.to_string()).collect(),
        );
        for dep in active_deps {
            systemd.active_set.borrow_mut().insert(dep.to_string());
        }
        if is_active {
            systemd
                .active_set
                .borrow_mut()
                .insert("app.service".to_string());
        }
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);

        assert_eq!(plan.activates_file("app.container"), expected);
        // Nothing was executed by planning alone.
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn plan_activates_dependency_of_a_started_unit() {
        // `web.image` is required by `web.service`, which is inactive but
        // wanted by an active `default.target`. Starting `web.service` pulls
        // `web-image.service` into the same transaction, so its image has to
        // be pre-pulled even though sync never names it.
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
        systemd.reverse_deps_map.borrow_mut().insert(
            "web.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "web-image.service".to_string(),
            vec!["web.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["web.container".into(), "web.image".into()],
            &cfg,
        );

        assert!(plan.activates_file("web.container"));
        assert!(plan.activates_file("web.image"));

        // systemd starts the image unit as a dependency; sync must not name
        // it itself.
        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["web.service"]);
    }

    #[test]
    fn plan_activates_transitive_dependency_chain() {
        // c.service <- b.service <- a.service, with only a.service wanted by
        // an active target. All three end up running.
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
        systemd
            .reverse_deps_map
            .borrow_mut()
            .insert("a.service".to_string(), vec!["default.target".to_string()]);
        systemd
            .reverse_deps_map
            .borrow_mut()
            .insert("b.service".to_string(), vec!["a.service".to_string()]);
        systemd
            .reverse_deps_map
            .borrow_mut()
            .insert("c.service".to_string(), vec!["b.service".to_string()]);
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &[
                "c.container".into(),
                "b.container".into(),
                "a.container".into(),
            ],
            &cfg,
        );

        assert!(plan.activates_file("a.container"));
        assert!(plan.activates_file("b.container"));
        assert!(plan.activates_file("c.container"));
    }

    #[test]
    fn plan_does_not_activate_dependency_of_a_skipped_unit() {
        // Nothing active wants `web.service`, so neither it nor the image
        // unit it requires will run.
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "web.service".to_string(),
            vec!["stopped.target".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "web-image.service".to_string(),
            vec!["web.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["web.container".into(), "web.image".into()],
            &cfg,
        );

        assert!(!plan.activates_file("web.container"));
        assert!(!plan.activates_file("web.image"));
    }

    #[test]
    fn plan_logs_nothing_until_executed() {
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["stopped.target".to_string()],
        );

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        assert!(
            err_buf.captured().is_empty(),
            "planning must be silent so it can be repeated around a pull"
        );

        execute_activation(&systemd, &plan, &cfg);
        assert!(
            err_buf.captured().contains("Skipping inactive app.service"),
            "expected skip log in: {}",
            err_buf.captured()
        );
    }

    #[test]
    fn plan_activates_template_file_only_with_running_instances() {
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["myapp@.container".into(), "other@.container".into()],
            &cfg,
        );

        assert!(plan.activates_file("myapp@.container"));
        assert!(!plan.activates_file("other@.container"));
    }

    #[test]
    fn plan_does_not_activate_unchanged_file() {
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);

        assert!(plan.activates_file("app.container"));
        assert!(!plan.activates_file("other.container"));
    }

    #[test]
    fn plan_maps_nested_path_to_same_unit() {
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["nested/app.container".into()], &cfg);

        assert!(plan.activates_file("nested/app.container"));
    }

    #[test]
    fn execute_activation_runs_planned_units() {
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd
            .active_set
            .borrow_mut()
            .insert("web.service".to_string());
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["app.container".into(), "web.service".into()],
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
        assert_eq!(systemd.restarted.borrow().as_slice(), &["web.service"]);
    }

    #[test]
    fn activate_active_unit_without_deps_restarts() {
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        assert!(systemd
            .restarted
            .borrow()
            .contains(&"app.service".to_string()));
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn activate_template_restarts_instances() {
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec![
                "myapp@web.service".to_string(),
                "myapp@worker.service".to_string(),
            ],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["myapp@.container".into()], &cfg);

        let restarted = systemd.restarted.borrow();
        assert!(restarted.contains(&"myapp@web.service".to_string()));
        assert!(restarted.contains(&"myapp@worker.service".to_string()));
    }

    #[test]
    fn activate_verbose_logs_actions() {
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "new.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
        systemd
            .active_set
            .borrow_mut()
            .insert("running.service".to_string());

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
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
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
    fn unit_state_is_failure_classifies_states() {
        use super::super::systemd::UnitState;
        let mk = |s: &str| UnitState {
            active_state: s.to_string(),
            sub_state: "any".to_string(),
            result: "any".to_string(),
            need_daemon_reload: false,
            n_restarts: 0,
            active_enter_timestamp_monotonic: None,
            fragment_path: None,
        };
        assert!(!mk("active").is_failure());
        assert!(!mk("activating").is_failure());
        assert!(mk("failed").is_failure());
        assert!(mk("inactive").is_failure());
        assert!(mk("deactivating").is_failure());
        assert!(UnitState::unknown().is_failure());
    }

    #[test]
    fn activate_reports_failed_state_and_returns_failures() {
        use super::super::systemd::UnitState;
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
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
        use super::super::systemd::UnitState;
        let systemd = MockSystemd::new();
        systemd
            .active_set
            .borrow_mut()
            .insert("default.target".to_string());
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
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
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
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
        systemd
            .active_set
            .borrow_mut()
            .insert("data-volume.service".to_string());
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
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
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
        systemd
            .active_set
            .borrow_mut()
            .insert("app.service".to_string());
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
