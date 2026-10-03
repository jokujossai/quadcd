use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use crate::config::Config;
use crate::install::{find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};

use super::systemd::ActivationState;
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

/// What sync will start and restart. Planned before execution so images can be
/// pre-pulled only for units that will run.
#[derive(Debug, Default)]
pub(crate) struct ActivationPlan {
    /// Inactive units to `systemctl start`.
    to_start: Vec<String>,
    /// Active units and running template instances to `systemctl restart`.
    to_restart: Vec<String>,
    /// Units that will be running afterwards, including ones systemd pulls in
    /// as dependencies. Templates appear un-instantiated. Also the pre-pull set.
    activating: HashSet<String>,
    /// `StartOnSync=` units: started when inactive, and activated before the rest.
    start_on_sync: HashSet<String>,
    /// Verbose output, printed at execution so planning twice logs once.
    notes: Vec<String>,
}

impl ActivationPlan {
    /// Record a verbose note; formatting is skipped when not verbose.
    fn note(&mut self, cfg: &Config, message: impl FnOnce() -> String) {
        if cfg.verbose {
            self.notes.push(message());
        }
    }

    /// Will the unit backing `filename` be running after this plan?
    pub(crate) fn activates_file(&self, filename: &str) -> bool {
        self.activating.contains(&unit_name_for_restart(filename))
    }
}

/// Decide how each changed unit is activated, mirroring what a reboot would
/// run. Must run after `daemon-reload`. Read-only; logs nothing.
///
/// - Active or crash-looping: restart.
/// - Already coming up (activating, or queued start job): leave to its job.
/// - Inactive: start if `StartOnSync=`, or if a unit that wants it (see
///   `START_AUTHORISING_PROPERTIES`) is coming up. A boot target is still
///   inactive with a queued job during the first sync, so jobs count too.
/// - Template: each loaded instance by the rules above.
///
/// Known gaps: a unit stopped by hand while the boot transaction that wants it
/// is still queued is started again; inactive socket/timer/path-activated
/// services are not pre-pulled; a unit still active with a queued stop job
/// counts as running.
pub(crate) fn plan_activation(
    systemd: &dyn SystemdTrait,
    changed_files: &[String],
    start_on_sync: &HashSet<String>,
    cfg: &Config,
) -> ActivationPlan {
    let mut units: Vec<String> = changed_files
        .iter()
        .map(|f| unit_name_for_restart(f))
        .collect();
    units.sort();
    units.dedup();

    let mut plan = ActivationPlan {
        start_on_sync: start_on_sync.clone(),
        ..ActivationPlan::default()
    };
    // Skipped units and their reverse deps; revisited by mark_transitively_activated.
    let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
    // Instance -> template, so an activated instance marks its template file.
    let mut templates: HashMap<String, String> = HashMap::new();
    let mut active = ActiveStates::new(systemd);

    for unit in &units {
        if is_template_unit(unit) {
            let pattern = unit.replace("@.", "@*.");
            let instances = systemd.list_units_matching(&pattern, cfg);
            if instances.is_empty() {
                plan.note(cfg, || format!("Template {unit}: no loaded instances"));
                continue;
            }
            let mut restarting: Vec<String> = Vec::new();
            for instance in instances {
                templates.insert(instance.clone(), unit.clone());
                if plan_unit(
                    systemd,
                    &instance,
                    &templates,
                    &mut plan,
                    &mut active,
                    &mut skipped,
                    cfg,
                ) == Action::Restart
                {
                    restarting.push(instance);
                }
            }
            if !restarting.is_empty() {
                plan.note(cfg, || {
                    format!(
                        "Template {unit}: restarting active instances: {}",
                        restarting.join(", ")
                    )
                });
            }
            continue;
        }

        plan_unit(
            systemd,
            unit,
            &templates,
            &mut plan,
            &mut active,
            &mut skipped,
            cfg,
        );
    }

    mark_transitively_activated(&mut plan, &mut skipped, &templates, cfg);

    for (unit, deps) in &skipped {
        plan.note(cfg, || {
            format!(
                "Skipping inactive {unit} (nothing that would start it is coming up; would be started by: [{}])",
                deps.join(", ")
            )
        });
    }

    plan
}

/// What [`plan_unit`] decided for one unit.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Running: restart into the new config.
    Restart,
    /// Inactive and wanted: start.
    Start,
    /// Left to the job systemd already has in flight; still counts as activated.
    AlreadyStarting,
    /// Inactive and nothing would start it: left alone.
    Skip,
}

/// Plan one concrete unit or template instance.
fn plan_unit(
    systemd: &dyn SystemdTrait,
    unit: &str,
    templates: &HashMap<String, String>,
    plan: &mut ActivationPlan,
    active: &mut ActiveStates,
    skipped: &mut Vec<(String, Vec<String>)>,
    cfg: &Config,
) -> Action {
    if active.is_active(unit, cfg) {
        plan.to_restart.push(unit.to_string());
        mark_activating(plan, templates, unit);
        return Action::Restart;
    }

    // Restart backoff has no job in flight, so restarting interrupts nothing
    // and applies the new config now; no later sync would revisit this file.
    if active.state(unit, cfg).is_auto_restarting() {
        plan.to_restart.push(unit.to_string());
        mark_activating(plan, templates, unit);
        return Action::Restart;
    }

    // A restart would abort the start underway and a start would merge into
    // it. The unit comes up with the old config, but its image is pre-pulled.
    if active.is_coming_up(unit, cfg) {
        let state = active.state(unit, cfg);
        plan.note(cfg, || {
            let how = if state.is_starting() {
                format!("{} ({})", state.active_state, state.sub_state)
            } else {
                "queued start job".to_string()
            };
            format!("{unit} is already coming up ({how}); leaving systemd's job alone")
        });
        mark_activating(plan, templates, unit);
        return Action::AlreadyStarting;
    }

    if plan.start_on_sync.contains(unit) {
        plan.note(cfg, || format!("Starting inactive {unit} (StartOnSync=)"));
        plan.to_start.push(unit.to_string());
        mark_activating(plan, templates, unit);
        return Action::Start;
    }

    // Start only if boot would: something that wants this unit is coming up.
    let deps = systemd.reverse_deps(unit, cfg);
    if deps.iter().any(|dep| active.authorises_start(dep, cfg)) {
        plan.to_start.push(unit.to_string());
        mark_activating(plan, templates, unit);
        Action::Start
    } else {
        skipped.push((unit.to_string(), deps));
        Action::Skip
    }
}

/// Record `unit` as running after the plan, and its template if it has one.
fn mark_activating(plan: &mut ActivationPlan, templates: &HashMap<String, String>, unit: &str) {
    plan.activating.insert(unit.to_string());
    if let Some(template) = templates.get(unit) {
        plan.activating.insert(template.clone());
    }
}

/// Unit states and the job list, each fetched at most once per planning pass.
struct ActiveStates<'a> {
    systemd: &'a dyn SystemdTrait,
    states: HashMap<String, ActivationState>,
    queued_start_jobs: Option<HashSet<String>>,
}

impl<'a> ActiveStates<'a> {
    fn new(systemd: &'a dyn SystemdTrait) -> Self {
        Self {
            systemd,
            states: HashMap::new(),
            queued_start_jobs: None,
        }
    }

    fn state(&mut self, unit: &str, cfg: &Config) -> ActivationState {
        if let Some(known) = self.states.get(unit) {
            return known.clone();
        }
        let state = self.systemd.activation_state(unit, cfg);
        self.states.insert(unit.to_string(), state.clone());
        state
    }

    /// Running, by `systemctl is-active`'s rule.
    fn is_active(&mut self, unit: &str, cfg: &Config) -> bool {
        self.state(unit, cfg).is_active()
    }

    /// Has a process or container behind it right now.
    fn is_running_or_starting(&mut self, unit: &str, cfg: &Config) -> bool {
        let state = self.state(unit, cfg);
        state.is_active() || state.is_starting()
    }

    fn has_queued_start_job(&mut self, unit: &str, cfg: &Config) -> bool {
        self.queued_start_jobs
            .get_or_insert_with(|| self.systemd.pending_start_jobs(cfg).into_iter().collect())
            .contains(unit)
    }

    /// Running, activating, or inactive with a queued start job (how a boot
    /// target looks during the first sync).
    fn is_coming_up(&mut self, unit: &str, cfg: &Config) -> bool {
        self.is_running_or_starting(unit, cfg) || self.has_queued_start_job(unit, cfg)
    }

    /// [`Self::is_coming_up`] minus restart backoff, so a crash-looping unit
    /// cannot resurrect what it wants.
    fn authorises_start(&mut self, unit: &str, cfg: &Config) -> bool {
        let state = self.state(unit, cfg);
        if state.is_auto_restarting() {
            return false;
        }
        state.is_active() || state.is_starting() || self.has_queued_start_job(unit, cfg)
    }
}

/// Mark skipped units that systemd will start as dependencies of activated ones
/// (e.g. a `.image` required by a `.container`), so their images get pulled.
/// Only chains through changed units are followed.
fn mark_transitively_activated(
    plan: &mut ActivationPlan,
    skipped: &mut Vec<(String, Vec<String>)>,
    templates: &HashMap<String, String>,
    cfg: &Config,
) {
    // A restart also starts the unit's dependencies.
    let mut pending: Vec<String> = plan
        .to_start
        .iter()
        .chain(plan.to_restart.iter())
        .cloned()
        .collect();

    while let Some(activated) = pending.pop() {
        let mut i = 0;
        while i < skipped.len() {
            if skipped[i].1.contains(&activated) {
                let (unit, _) = skipped.remove(i);
                plan.note(cfg, || {
                    format!("{unit}: started by systemd as a dependency of {activated}")
                });
                mark_activating(plan, templates, &unit);
                pending.push(unit);
            } else {
                i += 1;
            }
        }
    }
}

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
    let plan = plan_activation(systemd, changed_files, &HashSet::new(), cfg);
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
    use rstest::rstest;
    use std::fs;

    use super::super::systemd::testing::MockSystemd;

    /// Plan with no `StartOnSync=` units, which is what most tests need.
    /// Shadows the glob-imported function of the same name.
    fn plan_activation(
        systemd: &dyn SystemdTrait,
        changed_files: &[String],
        cfg: &Config,
    ) -> ActivationPlan {
        super::plan_activation(systemd, changed_files, &HashSet::new(), cfg)
    }

    fn start_on_sync(units: &[&str]) -> HashSet<String> {
        units.iter().map(|u| u.to_string()).collect()
    }

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

    #[rstest]
    #[case::inactive_wanted_by_active_target(&["default.target"], &["default.target"], false, "start")]
    #[case::inactive_required_by_active_unit(&["consumer.service"], &["consumer.service"], false, "start")]
    #[case::inactive_wanted_by_inactive_unit(&["consumer.service"], &[], false, "skip")]
    #[case::inactive_one_of_many_deps_active(&["a.service", "b.target"], &["b.target"], false, "start")]
    // `BoundBy` (reverse of `BindsTo=`): the binder starts this unit exactly
    // as a requirer would, so an active binder authorises the start.
    #[case::inactive_bound_by_active_unit(&["binder.service"], &["binder.service"], false, "start")]
    #[case::inactive_bound_by_inactive_unit(&["binder.service"], &[], false, "skip")]
    // `UpheldBy` (reverse of `Upholds=`): an active upholder restarts this
    // unit continuously while inactive or failed, so leaving it stopped would
    // be a state systemd itself refuses to hold.
    #[case::inactive_upheld_by_active_unit(&["supervisor.service"], &["supervisor.service"], false, "start")]
    #[case::inactive_upheld_by_inactive_unit(&["supervisor.service"], &[], false, "skip")]
    #[case::inactive_without_deps_skipped(&[], &[], false, "skip")]
    #[case::active_without_deps_restarts(&[], &[], true, "restart")]
    #[case::active_with_inactive_deps_restarts(&["consumer.service"], &[], true, "restart")]
    // The mock cannot tell properties apart; case names record intent.
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
            systemd.set_active(dep);
        }
        if is_active {
            systemd.set_active("app.service");
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

    // Which properties count (no PartOf=, TriggeredBy, ...) is tested in
    // sync::systemd and tests/fake_systemd.rs, not here.

    #[test]
    fn plan_activates_unit_upheld_by_a_started_unit() {
        // Starting web.service brings up helper.service, which it upholds.
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "web.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "helper.service".to_string(),
            vec!["web.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["web.container".into(), "helper.container".into()],
            &cfg,
        );

        assert!(plan.activates_file("web.container"));
        assert!(plan.activates_file("helper.container"));

        // systemd upholds it; sync must not name it in the start command.
        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["web.service"]);
    }

    // Reverse dependencies that are coming up (boot-time behaviour)

    #[test]
    fn activate_inactive_unit_wanted_by_target_with_queued_job_starts() {
        // A booting target is inactive with a queued start job.
        let systemd = MockSystemd::new();
        systemd.queue_start_job("default.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        assert!(
            plan.activates_file("app.container"),
            "a dependency with a queued start job must mark the unit for activation so its image is pre-pulled"
        );

        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn activate_inactive_unit_wanted_by_activating_service_starts() {
        // Services — unlike targets — do report `activating` while starting.
        let systemd = MockSystemd::new();
        systemd.set_activating("consumer.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["consumer.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(plan.activates_file("app.container"));
        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
    }

    #[test]
    fn activate_inactive_unit_wanted_by_inactive_target_still_skipped() {
        // The invariant this logic protects: a unit whose dependants are all
        // stopped — the shape an operator-stopped deployment has — is left
        // alone. A stopped unit is `inactive` with no job at all, so neither
        // the activating nor the queued-job check can resurrect it.
        let systemd = MockSystemd::new();
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["stopped.target".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(!plan.activates_file("app.container"));
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn activate_inactive_unit_whose_dependency_holds_an_unrelated_job_is_skipped() {
        // Only the reverse dependency's own job counts. A job queued for some
        // other unit must not leak authorisation to everything else.
        let systemd = MockSystemd::new();
        systemd.queue_start_job("elsewhere.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["stopped.target".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(!plan.activates_file("app.container"));
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn activate_activating_unit_is_left_to_its_own_job_but_still_pre_pulled() {
        // A changed unit that is itself mid-start gets no command at all:
        // `restart` would tear down a start already underway and `start` would
        // just be merged into the job in flight. It is still recorded as
        // activating, because it will be running and its image is needed.
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
        systemd.set_activating("app.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["default.target".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        assert!(plan.activates_file("app.container"));

        execute_activation(&systemd, &plan, &cfg);
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn activate_activating_unit_with_no_reverse_deps_is_still_pre_pulled() {
        // Regression: this used to be skipped and lose its pre-pull.
        let systemd = MockSystemd::new();
        systemd.set_activating("app.service");
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        assert!(plan.activates_file("app.container"));

        execute_activation(&systemd, &plan, &cfg);
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());

        let stderr = err_buf.captured();
        assert!(
            stderr.contains("app.service is already coming up (activating (start))"),
            "expected an accurate note, got: {stderr}"
        );
        assert!(
            !stderr.contains("Skipping inactive app.service"),
            "a starting unit must not be reported as inactive: {stderr}"
        );
    }

    #[test]
    fn activate_unit_with_queued_start_job_is_left_alone_but_pre_pulled() {
        // Same case one step earlier: the job is queued but has not run, so
        // the unit is still `inactive`. Issuing a start would only join that
        // job; the image is still needed.
        let systemd = MockSystemd::new();
        systemd.queue_start_job("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(plan.activates_file("app.container"));
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn plan_queries_each_unit_state_once_and_the_job_list_once() {
        // Every question about a unit comes from one `ActiveState`, so the
        // same unit is never queried twice in a pass — as a changed unit, as a
        // reverse dependency of another, or as both. The job list is a single
        // whole-system call, so it is fetched at most once too.
        let systemd = MockSystemd::new();
        systemd.set_active("web.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["web.service".to_string(), "default.target".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "db.service".to_string(),
            vec!["web.service".to_string(), "default.target".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        plan_activation(
            &systemd,
            &[
                "app.container".into(),
                "db.container".into(),
                "web.container".into(),
            ],
            &cfg,
        );

        let log = systemd.call_log.borrow();
        for unit in ["app.service", "db.service", "web.service", "default.target"] {
            let queries = log
                .iter()
                .filter(|c| *c == &format!("state:{unit}"))
                .count();
            assert!(queries <= 1, "{unit} queried {queries} times; log: {log:?}");
        }
        assert!(
            log.iter().filter(|c| *c == "list-jobs").count() <= 1,
            "list-jobs must be fetched at most once per pass; log: {log:?}"
        );
        assert!(
            !log.iter().any(|c| c.starts_with("is-active:")),
            "planning must not mix in a second kind of state query; log: {log:?}"
        );
    }

    #[test]
    fn activate_reloading_unit_is_restarted() {
        // `systemctl is-active` exits 0 for `reloading`, so the unit is
        // running and must be restarted into its new configuration — not
        // treated as inactive and left to the reverse-dependency check.
        let systemd = MockSystemd::new();
        systemd.set_reloading("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        assert_eq!(systemd.restarted.borrow().as_slice(), &["app.service"]);
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn activate_restarts_a_changed_unit_that_is_itself_auto_restarting() {
        // Unlike the same state on a *dependant* (see the test below), a
        // crash-looping changed unit is not left `AlreadyStarting`: no job is
        // actually in flight, so restarting now is safe and is the only way
        // the new configuration is ever applied to it.
        let systemd = MockSystemd::new();
        systemd.set_auto_restarting("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert_eq!(systemd.restarted.borrow().as_slice(), &["app.service"]);
        assert!(systemd.started.borrow().is_empty());
        assert!(plan.activates_file("app.container"));
    }

    #[test]
    fn activate_does_not_start_for_an_auto_restarting_dependant() {
        // A crash-looping dependant must not resurrect a stopped unit.
        let systemd = MockSystemd::new();
        systemd.set_auto_restarting("flapper.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["flapper.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(!plan.activates_file("app.container"));
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn activate_starts_for_a_genuinely_starting_dependant() {
        // The other side of the auto-restart exclusion: a dependant that is
        // actually part-way through starting does authorise the start.
        let systemd = MockSystemd::new();
        systemd.set_activating("consumer.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["consumer.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
    }

    #[test]
    fn activate_starts_for_a_reloading_dependant() {
        // `reloading` is running, by `systemctl is-active`'s own rule.
        let systemd = MockSystemd::new();
        systemd.set_reloading("consumer.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "app.service".to_string(),
            vec!["consumer.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
    }

    #[test]
    fn activate_fresh_clone_during_boot_starts_units_and_marks_dependencies() {
        // Fresh host: everything changed, nothing running, boot target queued.
        let systemd = MockSystemd::new();
        systemd.queue_start_job("multi-user.target");
        systemd.reverse_deps_map.borrow_mut().insert(
            "web.service".to_string(),
            vec!["multi-user.target".to_string()],
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

        // Both files are pre-pull candidates: the container is started, the
        // image unit is dragged in by it.
        assert!(plan.activates_file("web.container"));
        assert!(plan.activates_file("web.image"));

        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["web.service"]);
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
            systemd.set_active(dep);
        }
        if is_active {
            systemd.set_active("app.service");
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
        systemd.set_active("default.target");
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
    fn plan_activates_dependency_of_a_restarted_template_instance() {
        // Restarting the running instance re-enqueues its `Requires=`, so the
        // image unit it depends on comes up with it and has to be pre-pulled.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        systemd.set_active("myapp@1.service");
        systemd.reverse_deps_map.borrow_mut().insert(
            "myapp-image.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["myapp@.container".into(), "myapp.image".into()],
            &cfg,
        );

        assert!(plan.activates_file("myapp.image"));
        assert!(plan.activates_file("myapp@.container"));
    }

    #[test]
    fn plan_does_not_activate_dependency_of_a_stopped_template_instance() {
        // The only instance is stopped and nothing active wants it, so
        // neither it nor the image unit it requires will run.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "myapp-image.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["myapp@.container".into(), "myapp.image".into()],
            &cfg,
        );

        assert!(!plan.activates_file("myapp@.container"));
        assert!(!plan.activates_file("myapp.image"));

        execute_activation(&systemd, &plan, &cfg);
        assert!(systemd.restarted.borrow().is_empty());
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn plan_activates_transitive_dependency_chain() {
        // c.service <- b.service <- a.service, with only a.service wanted by
        // an active target. All three end up running.
        let systemd = MockSystemd::new();
        systemd.set_active("default.target");
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
        systemd.set_active("myapp@1.service");
        // `stopped@1.service` is loaded but inactive: `list-units --all`
        // reports it, and sync must not treat it as something to activate.
        systemd.listed_units.borrow_mut().insert(
            "stopped@*.service".to_string(),
            vec!["stopped@1.service".to_string()],
        );
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &[
                "myapp@.container".into(),
                "other@.container".into(),
                "stopped@.container".into(),
            ],
            &cfg,
        );

        assert!(plan.activates_file("myapp@.container"));
        assert!(!plan.activates_file("other@.container"));
        assert!(!plan.activates_file("stopped@.container"));
    }

    #[test]
    fn plan_template_starts_stopped_instance_wanted_by_active_unit() {
        // A stopped instance an active unit wants is what boot would bring
        // up, so sync starts it — and the template file counts as activated
        // for pre-pull.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "myapp@1.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["myapp@.container".into()], &cfg);
        assert!(plan.activates_file("myapp@.container"));

        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["myapp@1.service"]);
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn plan_template_leaves_failed_instance_alone_unless_wanted() {
        // A failed instance is inactive as far as `is-active` is concerned,
        // so it follows the same rule as any other stopped unit: restarted
        // into the fixed configuration only when something active wants it.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@boom.service".to_string()],
        );
        // A failed unit is listed by `list-units --all` but `is-active`
        // reports false for it, so it is left `inactive`.
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["myapp@.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(!plan.activates_file("myapp@.container"));
        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());

        // Same instance, now wanted by an active target: it is started.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@boom.service".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "myapp@boom.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");

        let plan = plan_activation(&systemd, &["myapp@.container".into()], &cfg);
        execute_activation(&systemd, &plan, &cfg);

        assert!(plan.activates_file("myapp@.container"));
        assert_eq!(systemd.started.borrow().as_slice(), &["myapp@boom.service"]);
    }

    #[test]
    fn plan_activates_template_file_for_transitively_started_instance() {
        // `myapp@1.service` is stopped and only wanted by `web.service`,
        // itself stopped but wanted by an active `default.target`. Sync
        // starts `web.service`, systemd drags the instance in with it, so the
        // template file's image still has to be pre-pulled.
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec!["myapp@1.service".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "myapp@1.service".to_string(),
            vec!["web.service".to_string()],
        );
        systemd.reverse_deps_map.borrow_mut().insert(
            "web.service".to_string(),
            vec!["default.target".to_string()],
        );
        systemd.set_active("default.target");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(
            &systemd,
            &["myapp@.container".into(), "web.container".into()],
            &cfg,
        );

        assert!(plan.activates_file("myapp@.container"));
        assert!(plan.activates_file("web.container"));

        // Only the changed unit is named; systemd starts the instance.
        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["web.service"]);
        assert!(systemd.restarted.borrow().is_empty());
    }

    #[test]
    fn plan_does_not_activate_unchanged_file() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["app.container".into()], &cfg);

        assert!(plan.activates_file("app.container"));
        assert!(!plan.activates_file("other.container"));
    }

    #[test]
    fn plan_maps_nested_path_to_same_unit() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = plan_activation(&systemd, &["nested/app.container".into()], &cfg);

        assert!(plan.activates_file("nested/app.container"));
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
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app.service"]);
        assert_eq!(systemd.restarted.borrow().as_slice(), &["web.service"]);
    }

    // StartOnSync=

    #[test]
    fn plan_starts_inactive_start_on_sync_unit_without_reverse_deps() {
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = super::plan_activation(
            &systemd,
            &["app.build".into()],
            &start_on_sync(&["app-build.service"]),
            &cfg,
        );

        assert!(plan.activates_file("app.build"));
        execute_activation(&systemd, &plan, &cfg);
        assert_eq!(systemd.started.borrow().as_slice(), &["app-build.service"]);
    }

    #[test]
    fn plan_skips_inactive_unit_without_start_on_sync() {
        let systemd = MockSystemd::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = super::plan_activation(
            &systemd,
            &["app.build".into()],
            &start_on_sync(&["other-build.service"]),
            &cfg,
        );

        assert!(!plan.activates_file("app.build"));
        execute_activation(&systemd, &plan, &cfg);
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn plan_starts_failed_start_on_sync_unit() {
        let systemd = MockSystemd::new();
        systemd.set_state("app-build.service", "failed", "failed");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = super::plan_activation(
            &systemd,
            &["app.build".into()],
            &start_on_sync(&["app-build.service"]),
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        assert_eq!(systemd.started.borrow().as_slice(), &["app-build.service"]);
    }

    #[test]
    fn plan_leaves_coming_up_start_on_sync_unit_to_its_job() {
        let systemd = MockSystemd::new();
        systemd.set_activating("app-build.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let plan = super::plan_activation(
            &systemd,
            &["app.build".into()],
            &start_on_sync(&["app-build.service"]),
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        assert!(systemd.started.borrow().is_empty());
        assert!(systemd.restarted.borrow().is_empty());
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

        let plan = super::plan_activation(
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
    fn plan_verbose_notes_start_on_sync() {
        let err = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err.clone()));
        cfg.verbose = true;
        let systemd = MockSystemd::new();

        let plan = super::plan_activation(
            &systemd,
            &["app.build".into()],
            &start_on_sync(&["app-build.service"]),
            &cfg,
        );
        execute_activation(&systemd, &plan, &cfg);

        let out = err.captured();
        assert!(
            out.contains("Starting inactive app-build.service (StartOnSync=)"),
            "got: {out}"
        );
        assert!(
            out.contains("Starting units (StartOnSync, first): app-build.service"),
            "got: {out}"
        );
    }

    #[test]
    fn activate_active_unit_without_deps_restarts() {
        let systemd = MockSystemd::new();
        systemd.set_active("app.service");
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["app.container".into()], &cfg);

        assert!(systemd
            .restarted
            .borrow()
            .contains(&"app.service".to_string()));
        assert!(systemd.started.borrow().is_empty());
    }

    #[test]
    fn activate_template_restarts_running_instances_only() {
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec![
                "myapp@web.service".to_string(),
                "myapp@worker.service".to_string(),
                // Loaded but stopped — reported because `list-units` runs
                // with `--all`. An operator stopped it by hand; sync must
                // leave it alone.
                "myapp@batch.service".to_string(),
            ],
        );
        for unit in &["myapp@web.service", "myapp@worker.service"] {
            systemd.set_active(unit);
        }
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        activate_changed_units_inner(&systemd, &["myapp@.container".into()], &cfg);

        let restarted = systemd.restarted.borrow();
        assert!(restarted.contains(&"myapp@web.service".to_string()));
        assert!(restarted.contains(&"myapp@worker.service".to_string()));
        assert!(
            !restarted.contains(&"myapp@batch.service".to_string()),
            "stopped instance must not be restarted: {restarted:?}"
        );
        assert!(
            systemd.started.borrow().is_empty(),
            "nothing wants the stopped instance, so it must not be started"
        );
    }

    #[test]
    fn activate_template_verbose_logs_skipped_instance() {
        let systemd = MockSystemd::new();
        systemd.listed_units.borrow_mut().insert(
            "myapp@*.service".to_string(),
            vec![
                "myapp@web.service".to_string(),
                "myapp@batch.service".to_string(),
            ],
        );
        systemd.set_active("myapp@web.service");
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        cfg.verbose = true;

        activate_changed_units_inner(&systemd, &["myapp@.container".into()], &cfg);

        let stderr = err_buf.captured();
        assert!(
            stderr.contains(
                "Template myapp@.service: restarting active instances: myapp@web.service"
            ),
            "expected template restart log in: {stderr}"
        );
        assert!(
            stderr.contains("Skipping inactive myapp@batch.service"),
            "expected skip log in: {stderr}"
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
        use super::super::systemd::UnitState;
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
