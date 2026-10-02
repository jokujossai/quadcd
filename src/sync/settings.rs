//! Per-unit sync settings read from a unit file's `[X-QuadCD]` section.
//!
//! systemd ignores sections whose names start with `X-`, and Quadlet only
//! validates keys in its own sections, so the section can sit in any unit
//! file without affecting the unit itself. Settings are read straight from
//! the source files in the repo, the same way image pre-pull reads `Image=`,
//! and keyed by the systemd unit name the file produces.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::config::Config;
use crate::install::{find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};

use super::units::{is_template_unit, unit_name_for_restart};

/// Name of the section quadcd reads its settings from.
pub(crate) const SECTION: &str = "X-QuadCD";

/// Settings parsed from one unit file's `[X-QuadCD]` section.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct UnitSettings {
    /// Start the unit when it changes even though nothing coming up wants it.
    pub start_on_sync: bool,
}

/// Parse the `[X-QuadCD]` section of a unit file.
///
/// Returns the settings plus a warning for every key or value that was not
/// understood. Repeated keys follow systemd's rule: the last one wins. No
/// variable substitution is applied.
pub(crate) fn parse_settings(content: &str) -> (UnitSettings, Vec<String>) {
    let mut settings = UnitSettings::default();
    let mut warnings = Vec::new();
    let mut in_section = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name == SECTION;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("ignoring malformed line '{line}'"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "StartOnSync" => match parse_bool(value) {
                Some(b) => settings.start_on_sync = b,
                None => warnings.push(format!("invalid boolean StartOnSync={value}")),
            },
            _ => warnings.push(format!("unknown key {key}")),
        }
    }

    (settings, warnings)
}

/// Parse a boolean the way systemd does.
fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "yes" | "y" | "true" | "t" | "on" => Some(true),
        "0" | "no" | "n" | "false" | "f" | "off" => Some(false),
        _ => None,
    }
}

/// Collect the unit names that set `StartOnSync=true` across all source dirs.
///
/// Every unit file is scanned rather than only the changed ones, because on a
/// fresh clone the changed list holds bare filenames that cannot be located
/// in nested directories. `StartOnSync=` on a template is ignored with a
/// warning: sync has no way to know which instances to start.
pub(crate) fn start_on_sync_units(
    source_dirs: &[(PathBuf, HashMap<String, String>)],
    cfg: &Config,
) -> HashSet<String> {
    let mut units = HashSet::new();

    for (dir, _) in source_dirs {
        let files = find_files(dir, QUADLET_EXTENSIONS)
            .into_iter()
            .chain(find_files(dir, SYSTEMD_EXTENSIONS));
        for path in files {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let (settings, warnings) = parse_settings(&content);
            for warning in warnings {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: {}: [{SECTION}] {warning}",
                    path.display()
                );
            }
            if !settings.start_on_sync {
                continue;
            }
            let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let unit = unit_name_for_restart(filename);
            if is_template_unit(&unit) {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: {}: StartOnSync= is ignored on template {unit}",
                    path.display()
                );
                continue;
            }
            units.insert(unit);
        }
    }

    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;
    use crate::output::tests::TestWriter;
    use rstest::rstest;

    #[rstest]
    #[case::true_value("[X-QuadCD]\nStartOnSync=true\n", true)]
    #[case::yes_value("[X-QuadCD]\nStartOnSync = yes\n", true)]
    #[case::false_value("[X-QuadCD]\nStartOnSync=false\n", false)]
    #[case::last_wins("[X-QuadCD]\nStartOnSync=true\nStartOnSync=no\n", false)]
    #[case::no_section("[Build]\nImageTag=localhost/app\n", false)]
    #[case::other_section("[Unit]\nStartOnSync=true\n", false)]
    #[case::section_after_others(
        "[Build]\nImageTag=x\n[X-QuadCD]\n# comment\nStartOnSync=1\n[Install]\n",
        true
    )]
    fn parse_settings_start_on_sync(#[case] content: &str, #[case] expected: bool) {
        let (settings, warnings) = parse_settings(content);
        assert_eq!(settings.start_on_sync, expected);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn parse_settings_warns_about_unknown_and_invalid() {
        let (settings, warnings) = parse_settings("[X-QuadCD]\nStartOnSync=maybe\nFoo=bar\njunk\n");
        assert!(!settings.start_on_sync);
        assert_eq!(
            warnings,
            vec![
                "invalid boolean StartOnSync=maybe",
                "unknown key Foo",
                "ignoring malformed line 'junk'",
            ]
        );
    }

    #[test]
    fn parse_settings_does_not_substitute_variables() {
        let (settings, warnings) = parse_settings("[X-QuadCD]\nStartOnSync=${ENABLED}\n");
        assert!(!settings.start_on_sync);
        assert_eq!(warnings, vec!["invalid boolean StartOnSync=${ENABLED}"]);
    }

    #[test]
    fn start_on_sync_units_keys_by_unit_name_and_recurses() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("builds");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("app.build"),
            "[Build]\nImageTag=localhost/app\n[X-QuadCD]\nStartOnSync=true\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("job.service"),
            "[Service]\nType=oneshot\n[X-QuadCD]\nStartOnSync=yes\n",
        )
        .unwrap();
        fs::write(tmp.path().join("web.container"), "[Container]\nImage=x\n").unwrap();
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        let units = start_on_sync_units(&[(tmp.path().to_path_buf(), HashMap::new())], &cfg);

        let expected: HashSet<String> = ["app-build.service", "job.service"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(units, expected);
    }

    #[test]
    fn start_on_sync_units_ignores_templates_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("worker@.container"),
            "[Container]\nImage=x\n[X-QuadCD]\nStartOnSync=true\n",
        )
        .unwrap();
        let err = TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err.clone()));

        let units = start_on_sync_units(&[(tmp.path().to_path_buf(), HashMap::new())], &cfg);

        assert!(units.is_empty());
        assert!(
            err.captured()
                .contains("StartOnSync= is ignored on template worker@.service"),
            "got: {}",
            err.captured()
        );
    }
}
