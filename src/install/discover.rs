//! Unit file discovery, generated unit names, and duplicate detection.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;

/// File extensions recognised as Podman Quadlet unit types.
pub const QUADLET_EXTENSIONS: &[&str] = &[
    "container",
    "volume",
    "network",
    "kube",
    "image",
    "build",
    "pod",
    "artifact",
];

/// File extensions recognised as standard systemd unit types.
pub const SYSTEMD_EXTENSIONS: &[&str] = &[
    "service",
    "socket",
    "device",
    "mount",
    "automount",
    "swap",
    "target",
    "path",
    "timer",
    "slice",
    "scope",
];

/// Return sorted paths of all files in `source_dir` (recursively) whose
/// extension matches one of the given `extensions`.
///
/// Paths are sorted lexicographically by their full path so duplicate
/// basenames are processed deterministically.
pub fn find_files(source_dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(source_dir, extensions, &mut files);
    files.sort();
    files
}

fn collect_files(dir: &Path, extensions: &[&str], files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories (e.g. .git)
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            collect_files(&path, extensions, files);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    files.push(path);
                }
            }
        }
    }
}

/// Return the systemd unit name that a Quadlet file would generate.
///
/// For example, `app.container` → `app.service`, `data.volume` →
/// `data-volume.service`. Returns `None` for non-Quadlet extensions.
pub fn generated_unit_name(filename: &str) -> Option<String> {
    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext {
        "container" | "kube" => Some(format!("{stem}.service")),
        "volume" => Some(format!("{stem}-volume.service")),
        "network" => Some(format!("{stem}-network.service")),
        "image" => Some(format!("{stem}-image.service")),
        "build" => Some(format!("{stem}-build.service")),
        "pod" => Some(format!("{stem}-pod.service")),
        "artifact" => Some(format!("{stem}-artifact.service")),
        _ => None,
    }
}

/// Warn about duplicate unit filenames and Quadlet/systemd name collisions
/// across multiple source directories.
///
/// Two kinds of conflicts are detected:
/// 1. Two source files with the same filename. Source directories are handled
///    in lexicographic order and each directory is walked in lexicographic
///    path order, so the later path deterministically overwrites the earlier
///    one during install.
/// 2. A Quadlet file that would generate a `.service` unit whose name
///    collides with an explicit systemd `.service` file.
pub fn warn_duplicate_units(source_dirs: &[(PathBuf, HashMap<String, String>)], cfg: &Config) {
    // filename → source path of first occurrence
    let mut seen_quadlet: HashMap<String, PathBuf> = HashMap::new();
    let mut seen_systemd: HashMap<String, PathBuf> = HashMap::new();
    // generated unit name → (quadlet source filename, source path)
    let mut generated: HashMap<String, (String, PathBuf)> = HashMap::new();

    for (dir, _) in source_dirs {
        if !dir.exists() {
            continue;
        }

        for file in find_files(dir, QUADLET_EXTENSIONS) {
            let name = file.file_name().unwrap().to_string_lossy().to_string();
            if let Some(prev) = seen_quadlet.get(&name) {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: duplicate Quadlet file '{name}' at {} (overrides {})",
                    file.display(),
                    prev.display()
                );
            } else {
                seen_quadlet.insert(name.clone(), file.clone());
            }
            if let Some(unit) = generated_unit_name(&name) {
                generated.insert(unit, (name, file.clone()));
            }
        }

        for file in find_files(dir, SYSTEMD_EXTENSIONS) {
            let name = file.file_name().unwrap().to_string_lossy().to_string();
            if let Some(prev) = seen_systemd.get(&name) {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: duplicate systemd unit '{name}' at {} (overrides {})",
                    file.display(),
                    prev.display()
                );
            } else {
                seen_systemd.insert(name.clone(), file.clone());
            }
        }
    }

    // Check for Quadlet → systemd name collisions
    for (unit_name, (quadlet_file, quadlet_path)) in &generated {
        if let Some(systemd_dir) = seen_systemd.get(unit_name) {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Warning: Quadlet file '{quadlet_file}' at {} generates '{unit_name}' \
                 which conflicts with explicit systemd unit {}",
                quadlet_path.display(),
                systemd_dir.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // find_files

    #[test]
    fn find_files_filters_by_extension() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("app.container"), "").unwrap();
        fs::write(tmp.path().join("web.service"), "").unwrap();
        fs::write(tmp.path().join("readme.txt"), "").unwrap();

        let quadlet = find_files(tmp.path(), QUADLET_EXTENSIONS);
        assert_eq!(quadlet.len(), 1);
        assert!(quadlet[0].ends_with("app.container"));

        let systemd = find_files(tmp.path(), SYSTEMD_EXTENSIONS);
        assert_eq!(systemd.len(), 1);
        assert!(systemd[0].ends_with("web.service"));
    }

    #[test]
    fn find_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = find_files(tmp.path(), QUADLET_EXTENSIONS);
        assert!(result.is_empty());
    }

    #[test]
    fn find_files_nonexistent_dir() {
        let result = find_files(Path::new("/no/such/dir"), QUADLET_EXTENSIONS);
        assert!(result.is_empty());
    }

    #[test]
    fn find_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("c.container"), "").unwrap();
        fs::write(tmp.path().join("a.container"), "").unwrap();
        fs::write(tmp.path().join("b.container"), "").unwrap();

        let files = find_files(tmp.path(), QUADLET_EXTENSIONS);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.container", "b.container", "c.container"]);
    }

    #[test]
    fn find_files_sorts_full_paths_for_duplicate_names() {
        let tmp = tempfile::tempdir().unwrap();
        let left = tmp.path().join("left");
        let right = tmp.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        fs::write(left.join("dup.container"), "").unwrap();
        fs::write(right.join("dup.container"), "").unwrap();
        fs::write(left.join("other.container"), "").unwrap();

        let files = find_files(tmp.path(), QUADLET_EXTENSIONS);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(tmp.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "left/dup.container".to_string(),
                "left/other.container".to_string(),
                "right/dup.container".to_string()
            ]
        );
    }

    #[test]
    fn find_files_recurses_into_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("traefik");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("traefik.container"), "").unwrap();
        fs::write(sub.join("loadbalancer.network"), "").unwrap();
        fs::write(tmp.path().join("top.volume"), "").unwrap();

        let files = find_files(tmp.path(), QUADLET_EXTENSIONS);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["top.volume", "loadbalancer.network", "traefik.container"]
        );
    }

    #[test]
    fn find_files_skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("should-be-ignored.container"), "").unwrap();
        fs::write(tmp.path().join("visible.container"), "").unwrap();

        let files = find_files(tmp.path(), QUADLET_EXTENSIONS);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("visible.container"));
    }

    // generated_unit_name

    #[rstest]
    #[case::container("app.container", "app.service")]
    #[case::kube("k8s.kube", "k8s.service")]
    #[case::volume("data.volume", "data-volume.service")]
    #[case::network("net.network", "net-network.service")]
    #[case::image("img.image", "img-image.service")]
    #[case::build("b.build", "b-build.service")]
    #[case::pod("p.pod", "p-pod.service")]
    #[case::artifact("a.artifact", "a-artifact.service")]
    fn generated_unit_name_quadlet(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(generated_unit_name(input), Some(expected.to_string()));
    }

    #[rstest]
    #[case::service("app.service")]
    #[case::timer("app.timer")]
    #[case::txt("readme.txt")]
    fn generated_unit_name_non_quadlet(#[case] input: &str) {
        assert_eq!(generated_unit_name(input), None);
    }

    // warn_duplicate_units

    #[test]
    fn warn_duplicate_quadlet_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("repo-a");
        let dir_b = tmp.path().join("repo-b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_a.join("app.container"), "").unwrap();
        fs::write(dir_b.join("app.container"), "").unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        let source_dirs = vec![(dir_a, HashMap::new()), (dir_b, HashMap::new())];
        warn_duplicate_units(&source_dirs, &cfg);

        let err = err_buf.captured();
        assert!(
            err.contains("duplicate Quadlet file 'app.container'"),
            "got: {err}"
        );
        assert!(err.contains("overrides"), "got: {err}");
    }

    #[test]
    fn warn_duplicate_systemd_units() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("repo-a");
        let dir_b = tmp.path().join("repo-b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_a.join("app.service"), "").unwrap();
        fs::write(dir_b.join("app.service"), "").unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        let source_dirs = vec![(dir_a, HashMap::new()), (dir_b, HashMap::new())];
        warn_duplicate_units(&source_dirs, &cfg);

        let err = err_buf.captured();
        assert!(
            err.contains("duplicate systemd unit 'app.service'"),
            "got: {err}"
        );
        assert!(err.contains("overrides"), "got: {err}");
    }

    #[test]
    fn warn_quadlet_systemd_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("repo-a");
        let dir_b = tmp.path().join("repo-b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_a.join("app.container"), "").unwrap();
        fs::write(dir_b.join("app.service"), "").unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        let source_dirs = vec![(dir_a, HashMap::new()), (dir_b, HashMap::new())];
        warn_duplicate_units(&source_dirs, &cfg);

        let err = err_buf.captured();
        assert!(
            err.contains("generates 'app.service' which conflicts"),
            "got: {err}"
        );
        assert!(err.contains("explicit systemd unit"), "got: {err}");
    }

    #[test]
    fn warn_no_duplicates_no_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_a = tmp.path().join("repo-a");
        let dir_b = tmp.path().join("repo-b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_a.join("app.container"), "").unwrap();
        fs::write(dir_b.join("web.container"), "").unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        let source_dirs = vec![(dir_a, HashMap::new()), (dir_b, HashMap::new())];
        warn_duplicate_units(&source_dirs, &cfg);

        let err = err_buf.captured();
        assert!(err.is_empty(), "expected no warnings, got: {err}");
    }
}
