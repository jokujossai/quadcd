use std::path::Path;

use crate::install::{find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};

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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

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
}
