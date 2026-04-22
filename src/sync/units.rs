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

/// Activate changed units using smart start/restart logic.
///
/// After `daemon-reload`, each changed unit is inspected:
/// - **Templates** (`foo@.service`): discover running instances via
///   `list-units` and restart them.
/// - **Enabled / generated** units: `start` if not active, `restart` if active.
/// - **Static** units: `restart` only if currently active.
/// - **Disabled / masked** units: skipped.
pub(crate) fn activate_changed_units_inner(
    systemd: &dyn SystemdTrait,
    changed_files: &[String],
    cfg: &Config,
) {
    let mut units: Vec<String> = changed_files
        .iter()
        .map(|f| unit_name_for_restart(f))
        .collect();
    units.sort();
    units.dedup();

    if units.is_empty() {
        return;
    }

    let mut to_start: Vec<String> = Vec::new();
    let mut to_restart: Vec<String> = Vec::new();

    for unit in &units {
        if is_template_unit(unit) {
            // Discover running instances for this template
            let pattern = unit.replace("@.", "@*.");
            let instances = systemd.list_units_matching(&pattern, cfg);
            if cfg.verbose {
                if instances.is_empty() {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Template {unit}: no running instances found"
                    );
                } else {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Template {unit}: restarting instances: {}",
                        instances.join(", ")
                    );
                }
            }
            to_restart.extend(instances);
            continue;
        }

        let enabled = systemd.is_enabled(unit, cfg);
        let active = systemd.is_active(unit, cfg);

        // Treat any "enabled*" variant (enabled, enabled-runtime) plus
        // static and generated as startable units.
        let startable =
            enabled.starts_with("enabled") || enabled == "static" || enabled == "generated";

        match (startable, active) {
            (true, true) => to_restart.push(unit.clone()),
            (true, false) => to_start.push(unit.clone()),
            (false, true) => to_restart.push(unit.clone()),
            _ => {
                if cfg.verbose {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Skipping {unit} (is-enabled={enabled}, active={active})"
                    );
                }
            }
        }
    }

    if cfg.verbose {
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
        systemd.start(&to_start, cfg);
    }
    if !to_restart.is_empty() {
        systemd.restart(&to_restart, cfg);
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
            systemd
                .enabled_map
                .borrow_mut()
                .insert(unit.to_string(), "enabled".to_string());
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
    #[case::enabled_inactive_starts("enabled", false, "start")]
    #[case::enabled_active_restarts("enabled", true, "restart")]
    #[case::generated_starts("generated", false, "start")]
    #[case::enabled_runtime_starts("enabled-runtime", false, "start")]
    #[case::static_active_restarts("static", true, "restart")]
    #[case::static_inactive_starts("static", false, "start")]
    #[case::disabled_skipped("disabled", false, "skip")]
    #[case::masked_skipped("masked", false, "skip")]
    fn activate_unit_by_state(
        #[case] enabled_state: &str,
        #[case] is_active: bool,
        #[case] expected: &str,
    ) {
        let systemd = MockSystemd::new();
        if enabled_state != "disabled" {
            systemd
                .enabled_map
                .borrow_mut()
                .insert("app.service".to_string(), enabled_state.to_string());
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

    #[test]
    fn activate_not_enabled_but_active_restarts() {
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
        systemd
            .enabled_map
            .borrow_mut()
            .insert("new.service".to_string(), "enabled".to_string());
        systemd
            .enabled_map
            .borrow_mut()
            .insert("running.service".to_string(), "enabled".to_string());
        systemd
            .active_set
            .borrow_mut()
            .insert("running.service".to_string());
        systemd
            .enabled_map
            .borrow_mut()
            .insert("skip.service".to_string(), "disabled".to_string());

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
            stderr.contains("Skipping skip.service"),
            "expected skip log in: {stderr}"
        );
    }

    #[test]
    fn restart_verbose_logs_units() {
        let systemd = MockSystemd::new();
        systemd
            .enabled_map
            .borrow_mut()
            .insert("app.service".to_string(), "enabled".to_string());
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
}
