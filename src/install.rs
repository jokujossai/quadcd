//! File discovery, environment substitution, and installation of Quadlet and
//! systemd unit files.
//!
//! Quadlet files (`.container`, `.volume`, etc.) are installed into a
//! caller-specified directory that quadcd owns entirely. The caller is
//! responsible for clearing the directory before installing. Plain systemd
//! units (`.service`, `.timer`, etc.) are copied directly into the generator's
//! normal output directory.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
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

/// Perform `${VAR}` substitution in `content` using the provided key-value map.
///
/// Only the exact `${KEY}` syntax is replaced; `$KEY` without braces is not
/// matched. Substitution is single-pass: variables appearing in substituted
/// values are not expanded.
pub fn envsubst(content: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find(['}', '$', '{']);
        if end.is_some_and(|e| rest.as_bytes()[e] == b'}') {
            let end = end.unwrap();
            let key = &rest[..end];
            if let Some(value) = vars.get(key) {
                result.push_str(value);
            } else {
                result.push_str("${");
                result.push_str(key);
                result.push('}');
            }
            rest = &rest[end + 1..];
        } else {
            // No valid closing brace — emit the literal "${" and
            // continue scanning from where we are (so inner "${" can match).
            result.push_str("${");
        }
    }
    result.push_str(rest);
    result
}

/// Inject or replace `SourcePath=` in a unit file's `[Unit]` section.
///
/// If the content already has a `SourcePath=` line it is replaced.
/// If a `[Unit]` section exists, the directive is inserted right after it.
/// Otherwise a `[Unit]` section is prepended.
fn set_source_path(content: &str, source_path: &Path) -> String {
    let source_line = format!("SourcePath={}", source_path.display());
    let mut result = String::with_capacity(content.len() + source_line.len() + 20);
    let mut injected = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("SourcePath=") {
            continue;
        }

        result.push_str(line);
        result.push('\n');

        if !injected && trimmed == "[Unit]" {
            result.push_str(&source_line);
            result.push('\n');
            injected = true;
        }
    }

    if !injected {
        let mut prefixed = format!("[Unit]\n{source_line}\n\n");
        prefixed.push_str(&result);
        return prefixed;
    }

    result
}

/// Remove duplicate `SourcePath=` lines, keeping only the first occurrence.
///
/// The podman generator adds its own `SourcePath=` pointing at the temporary
/// quadlet directory. Because quadcd already injects the real `SourcePath=`
/// first, dropping duplicates preserves the correct value.
pub fn clean_duplicate_source_path(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut seen = false;

    for line in content.lines() {
        if line.trim().starts_with("SourcePath=") {
            if seen {
                continue;
            }
            seen = true;
        }
        result.push_str(line);
        result.push('\n');
    }

    result
}

/// Apply [`clean_duplicate_source_path`] to every file in `dir`.
pub fn clean_generated_source_paths(dir: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let cleaned = clean_duplicate_source_path(&content);
        if cleaned != content {
            write_atomic(&path, &cleaned)
                .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Write `content` to `dest` atomically by writing to a temporary file
/// in the same directory, then renaming.
fn write_atomic(dest: &Path, content: &str) -> Result<(), String> {
    let dir = dest
        .parent()
        .ok_or_else(|| format!("No parent directory for {}", dest.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("Failed to create temp file in {}: {e}", dir.display()))?;
    tmp.write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write temp file: {e}"))?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o644))
        .map_err(|e| format!("Failed to set permissions on temp file: {e}"))?;
    tmp.persist(dest)
        .map_err(|e| format!("Failed to persist temp file to {}: {e}", dest.display()))?;
    Ok(())
}

/// Install Quadlet unit files from `source_dir` into `quadlet_dir`.
///
/// The caller owns `quadlet_dir` and is responsible for clearing it before
/// the first call when a clean slate is needed.
pub fn install_quadlet_files(
    source_dir: &Path,
    quadlet_dir: &Path,
    env_vars: &HashMap<String, String>,
    cfg: &Config,
) -> Result<(), String> {
    let verbose = cfg.verbose;

    let files = find_files(source_dir, QUADLET_EXTENSIONS);
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy();
        if verbose {
            let _ = writeln!(cfg.output.err(), "[quadcd] Installing Quadlet file: {name}");
        }
        let content = fs::read_to_string(file)
            .map_err(|e| format!("Failed to read {}: {e}", file.display()))?;
        let content = envsubst(&content, env_vars);
        let content = set_source_path(&content, file);
        write_atomic(&quadlet_dir.join(name.as_ref()), &content)
            .map_err(|e| format!("Failed to write {name}: {e}"))?;
    }

    Ok(())
}

/// Install plain systemd unit files from `source_dir` directly into
/// `normal_dir`, applying environment variable substitution.
pub fn install_systemd_units(
    source_dir: &Path,
    normal_dir: &Path,
    env_vars: &HashMap<String, String>,
    cfg: &Config,
) -> Result<(), String> {
    fs::create_dir_all(normal_dir)
        .map_err(|e| format!("Failed to create unit dir {}: {e}", normal_dir.display()))?;

    let files = find_files(source_dir, SYSTEMD_EXTENSIONS);
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy();
        if cfg.verbose {
            let _ = writeln!(cfg.output.err(), "[quadcd] Installing systemd unit: {name}");
        }
        let content = fs::read_to_string(file)
            .map_err(|e| format!("Failed to read {}: {e}", file.display()))?;
        let content = envsubst(&content, env_vars);
        let content = set_source_path(&content, file);
        write_atomic(&normal_dir.join(name.as_ref()), &content)
            .map_err(|e| format!("Failed to write {name}: {e}"))?;
    }
    Ok(())
}

/// Create symbolic links in `quadlet_dir` for every `*.d` drop-in directory
/// found in `dropins_dir`.
///
/// This allows the Podman generator (invoked with `QUADLET_UNIT_DIRS` pointing
/// at `quadlet_dir`) to discover global and per-unit drop-in overrides such as
/// `container.d/` or `foo.container.d/`.
///
/// Existing entries in `quadlet_dir` with a conflicting name are skipped with
/// a warning.
pub fn symlink_dropins(dropins_dir: &Path, quadlet_dir: &Path, cfg: &Config) -> Result<(), String> {
    let entries = match fs::read_dir(dropins_dir) {
        Ok(e) => e,
        Err(e) => {
            return Err(format!(
                "Failed to read drop-in dir {}: {e}",
                dropins_dir.display()
            ));
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if !name.ends_with(".d") {
            continue;
        }

        let link = quadlet_dir.join(&name);
        if link.exists() || link.symlink_metadata().is_ok() {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Warning: skipping drop-in symlink '{name}': already exists in {}",
                quadlet_dir.display()
            );
            continue;
        }

        std::os::unix::fs::symlink(&path, &link).map_err(|e| {
            format!(
                "Failed to symlink {} -> {}: {e}",
                link.display(),
                path.display()
            )
        })?;

        if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Linked drop-in dir: {} -> {}",
                link.display(),
                path.display()
            );
        }
    }

    Ok(())
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

fn open_sync_lock_file(data_dir: &Path) -> Result<fs::File, String> {
    let lock_path = data_dir.join(".quadcd-sync.lock");
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open lock file {}: {e}", lock_path.display()))
}

/// Acquire an exclusive lock on `data_dir/.quadcd-sync.lock`, blocking until
/// any other holder releases it.
///
/// Returns the open `File` handle whose lifetime holds the lock. The lock is
/// released automatically when the handle is dropped.
pub fn acquire_sync_lock(data_dir: &Path) -> Result<fs::File, String> {
    let file = open_sync_lock_file(data_dir)?;
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("Failed to acquire sync lock: {err}"));
    }
    Ok(file)
}

/// Try to acquire an exclusive lock on `data_dir/.quadcd-sync.lock` without
/// blocking.
///
/// Returns `Ok(Some(file))` when the lock was acquired, `Ok(None)` when another
/// process already holds it, or `Err(_)` on a real I/O failure.
pub fn try_acquire_sync_lock(data_dir: &Path) -> Result<Option<fs::File>, String> {
    let file = open_sync_lock_file(data_dir)?;
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(file));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(format!("Failed to acquire sync lock: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // envsubst

    #[test]
    fn envsubst_replaces_variables() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "world".to_string());
        vars.insert("PORT".to_string(), "8080".to_string());
        let result = envsubst("Hello ${NAME} on port ${PORT}", &vars);
        assert_eq!(result, "Hello world on port 8080");
    }

    #[test]
    fn envsubst_missing_var_left_as_is() {
        let vars = HashMap::new();
        let result = envsubst("${MISSING} stays", &vars);
        assert_eq!(result, "${MISSING} stays");
    }

    #[test]
    fn envsubst_empty_vars_map() {
        let vars = HashMap::new();
        let result = envsubst("no vars here", &vars);
        assert_eq!(result, "no vars here");
    }

    #[test]
    fn envsubst_bare_dollar_not_replaced() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = envsubst("$FOO and ${FOO}", &vars);
        assert_eq!(result, "$FOO and bar");
    }

    #[test]
    fn envsubst_multiple_occurrences() {
        let mut vars = HashMap::new();
        vars.insert("X".to_string(), "y".to_string());
        let result = envsubst("${X}${X}${X}", &vars);
        assert_eq!(result, "yyy");
    }

    #[test]
    fn envsubst_empty_value() {
        let mut vars = HashMap::new();
        vars.insert("EMPTY".to_string(), String::new());
        let result = envsubst("before${EMPTY}after", &vars);
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn envsubst_cascading_var_in_value_not_expanded() {
        let mut vars = HashMap::new();
        vars.insert("IMAGE_TAG".to_string(), "latest".to_string());
        vars.insert(
            "IMAGE".to_string(),
            "quay.io/podman/hello:${IMAGE_TAG}".to_string(),
        );
        let result = envsubst("Image=${IMAGE}", &vars);
        assert_eq!(result, "Image=quay.io/podman/hello:${IMAGE_TAG}");
    }

    #[test]
    fn envsubst_deterministic_regardless_of_insertion_order() {
        let mut vars1 = HashMap::new();
        vars1.insert("A".to_string(), "${B}".to_string());
        vars1.insert("B".to_string(), "hello".to_string());

        // Run many times to exercise different HashMap orderings
        let results: std::collections::HashSet<String> =
            (0..50).map(|_| envsubst("${A} ${B}", &vars1)).collect();
        assert_eq!(results.len(), 1, "envsubst must be deterministic");
        assert_eq!(results.into_iter().next().unwrap(), "${B} hello");
    }

    #[test]
    fn envsubst_malformed_no_closing_brace() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = envsubst("${FOO} and ${NOCLOSE", &vars);
        assert_eq!(result, "bar and ${NOCLOSE");
    }

    #[test]
    fn envsubst_nested_dollar_brace_skips_outer() {
        let mut vars = HashMap::new();
        vars.insert("test".to_string(), "value".to_string());
        assert_eq!(envsubst("${${test}", &vars), "${value");
    }

    #[test]
    fn envsubst_adjacent_vars() {
        let mut vars = HashMap::new();
        vars.insert("A".to_string(), "1".to_string());
        vars.insert("B".to_string(), "2".to_string());
        assert_eq!(envsubst("${A}${B}", &vars), "12");
    }

    #[test]
    fn envsubst_preserves_utf8() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "wörld".to_string());
        let result = envsubst("héllo ${NAME} café", &vars);
        assert_eq!(result, "héllo wörld café");
    }

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

    use rstest::rstest;

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

    // write_atomic

    #[test]
    fn write_atomic_creates_file_with_content() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("output.txt");
        write_atomic(&dest, "hello atomically").unwrap();
        let content = fs::read_to_string(&dest).unwrap();
        assert_eq!(content, "hello atomically");
    }

    #[test]
    fn write_atomic_overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("output.txt");
        fs::write(&dest, "old content").unwrap();
        write_atomic(&dest, "new content").unwrap();
        let content = fs::read_to_string(&dest).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn write_atomic_sets_readable_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("output.txt");
        write_atomic(&dest, "content").unwrap();
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    // set_source_path

    #[test]
    fn set_source_path_injects_after_unit_section() {
        let content = "[Unit]\nDescription=test\n\n[Service]\nExecStart=/bin/true\n";
        let result = set_source_path(content, Path::new("/src/app.container"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/app.container\nDescription=test\n\n[Service]\nExecStart=/bin/true\n"
        );
    }

    #[test]
    fn set_source_path_prepends_unit_section_when_missing() {
        let content = "[Service]\nExecStart=/bin/true\n";
        let result = set_source_path(content, Path::new("/src/app.service"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/app.service\n\n[Service]\nExecStart=/bin/true\n"
        );
    }

    #[test]
    fn set_source_path_replaces_existing() {
        let content = "[Unit]\nSourcePath=/tmp/old\nDescription=test\n";
        let result = set_source_path(content, Path::new("/src/real.container"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/real.container\nDescription=test\n"
        );
    }

    #[test]
    fn set_source_path_no_content() {
        let result = set_source_path("", Path::new("/src/app.container"));
        assert_eq!(result, "[Unit]\nSourcePath=/src/app.container\n\n");
    }

    // clean_duplicate_source_path

    #[test]
    fn clean_duplicate_source_path_keeps_first() {
        let content = "[Unit]\nSourcePath=/real/path\nSourcePath=/tmp/fake\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nSourcePath=/real/path\nDescription=test\n");
    }

    #[test]
    fn clean_duplicate_source_path_single_is_kept() {
        let content = "[Unit]\nSourcePath=/only/one\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nSourcePath=/only/one\nDescription=test\n");
    }

    #[test]
    fn clean_duplicate_source_path_none_is_noop() {
        let content = "[Unit]\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nDescription=test\n");
    }

    #[test]
    fn install_quadlet_files_prefers_later_duplicate_path() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let quadlet_dir = tmp.path().join("quadlet");
        fs::create_dir_all(source.join("alpha")).unwrap();
        fs::create_dir_all(source.join("beta")).unwrap();
        fs::create_dir_all(&quadlet_dir).unwrap();

        fs::write(
            source.join("alpha/dup.container"),
            "[Container]\nImage=alpha\n",
        )
        .unwrap();
        fs::write(
            source.join("beta/dup.container"),
            "[Container]\nImage=beta\n",
        )
        .unwrap();

        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        install_quadlet_files(&source, &quadlet_dir, &HashMap::new(), &cfg).unwrap();

        let installed = fs::read_to_string(quadlet_dir.join("dup.container")).unwrap();
        assert!(installed.contains("Image=beta"), "content: {installed}");
        assert!(
            installed.contains("SourcePath=") && installed.contains("beta/dup.container"),
            "content: {installed}"
        );
    }

    // symlink_dropins

    #[test]
    fn symlink_dropins_links_dot_d_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dropins = tmp.path().join("dropins");
        let quadlet = tmp.path().join("quadlet");
        fs::create_dir_all(&dropins).unwrap();
        fs::create_dir_all(&quadlet).unwrap();

        // Create drop-in directories
        fs::create_dir_all(dropins.join("container.d")).unwrap();
        fs::write(
            dropins.join("container.d/10-defaults.conf"),
            "[Container]\nLogDriver=journald\n",
        )
        .unwrap();
        fs::create_dir_all(dropins.join("myapp.container.d")).unwrap();
        fs::write(
            dropins.join("myapp.container.d/20-override.conf"),
            "[Container]\nVolume=/data:/data\n",
        )
        .unwrap();

        // Non-.d entries should be ignored
        fs::create_dir_all(dropins.join("notadropin")).unwrap();
        fs::write(dropins.join("somefile.conf"), "ignored").unwrap();

        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        symlink_dropins(&dropins, &quadlet, &cfg).unwrap();

        // Symlinks should exist
        let link1 = quadlet.join("container.d");
        let link2 = quadlet.join("myapp.container.d");
        assert!(link1.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(link2.symlink_metadata().unwrap().file_type().is_symlink());

        // Should resolve to the original content
        let content = fs::read_to_string(link1.join("10-defaults.conf")).unwrap();
        assert!(content.contains("LogDriver=journald"));

        // Non-.d dirs should not be linked
        assert!(!quadlet.join("notadropin").exists());
        assert!(!quadlet.join("somefile.conf").exists());
    }

    #[test]
    fn symlink_dropins_skips_existing_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let dropins = tmp.path().join("dropins");
        let quadlet = tmp.path().join("quadlet");
        fs::create_dir_all(&dropins).unwrap();
        fs::create_dir_all(&quadlet).unwrap();

        fs::create_dir_all(dropins.join("container.d")).unwrap();
        // Pre-existing entry in quadlet dir
        fs::create_dir_all(quadlet.join("container.d")).unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));
        symlink_dropins(&dropins, &quadlet, &cfg).unwrap();

        let err = err_buf.captured();
        assert!(
            err.contains("skipping drop-in symlink 'container.d'"),
            "got: {err}"
        );
    }

    #[test]
    fn symlink_dropins_empty_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dropins = tmp.path().join("dropins");
        let quadlet = tmp.path().join("quadlet");
        fs::create_dir_all(&dropins).unwrap();
        fs::create_dir_all(&quadlet).unwrap();

        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        symlink_dropins(&dropins, &quadlet, &cfg).unwrap();
    }
}
