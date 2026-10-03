//! File discovery, environment substitution, and installation of Quadlet and
//! systemd unit files.
//!
//! Quadlet files (`.container`, `.volume`, etc.) are installed into a
//! caller-specified directory that quadcd owns entirely. The caller is
//! responsible for clearing the directory before installing. Plain systemd
//! units (`.service`, `.timer`, etc.) are copied directly into the generator's
//! normal output directory.

mod content;
mod discover;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::config::Config;

use content::set_source_path;

pub use content::{clean_duplicate_source_path, envsubst};
pub use discover::{
    find_files, generated_unit_name, warn_duplicate_units, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS,
};

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
///
/// Like Quadlet, the `[Install]` section is honoured by materialising
/// `WantedBy=`/`RequiredBy=` as `<target>.wants/` and `<target>.requires/`
/// symlinks in `normal_dir` — generated units cannot be enabled with
/// `systemctl enable`, so this is the only way for them to start at boot.
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
        link_install_dependencies(&name, &content, normal_dir, cfg);
    }
    Ok(())
}

/// Dependency lists parsed from a unit's `[Install]` section.
#[derive(Debug, Default, PartialEq)]
struct InstallSection {
    wanted_by: Vec<String>,
    required_by: Vec<String>,
}

/// Parse `WantedBy=` and `RequiredBy=` from the `[Install]` section.
///
/// Values are space-separated and accumulate across repeated assignments;
/// an empty assignment resets the list (systemd semantics).
fn parse_install_section(content: &str) -> InstallSection {
    let mut section = InstallSection::default();
    let mut in_install = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_install = line == "[Install]";
            continue;
        }
        if !in_install {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let list = match key.trim() {
            "WantedBy" => &mut section.wanted_by,
            "RequiredBy" => &mut section.required_by,
            _ => continue,
        };
        let value = value.trim();
        if value.is_empty() {
            list.clear();
        } else {
            list.extend(value.split_whitespace().map(str::to_string));
        }
    }
    section
}

/// Create `<target>.wants/<unit>` and `<target>.requires/<unit>` symlinks in
/// `normal_dir` for the unit's `[Install]` dependencies.
fn link_install_dependencies(unit_name: &str, content: &str, normal_dir: &Path, cfg: &Config) {
    let install = parse_install_section(content);
    let deps = install
        .wanted_by
        .iter()
        .map(|t| (t, "wants"))
        .chain(install.required_by.iter().map(|t| (t, "requires")));
    for (target, kind) in deps {
        let dir = normal_dir.join(format!("{target}.{kind}"));
        let link = dir.join(unit_name);
        let result = fs::create_dir_all(&dir)
            .and_then(|()| std::os::unix::fs::symlink(Path::new("..").join(unit_name), &link));
        match result {
            Ok(()) => {
                if cfg.verbose {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Linking {unit_name} into {target}.{kind}/"
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: failed to link {unit_name} into {target}.{kind}/: {e}"
                );
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    // install_quadlet_files

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

    // parse_install_section

    #[test]
    fn parse_install_section_wanted_and_required() {
        let unit = "[Unit]\nDescription=x\n\n[Install]\nWantedBy=multi-user.target default.target\nRequiredBy=other.service\n";
        let section = parse_install_section(unit);
        assert_eq!(
            section.wanted_by,
            vec!["multi-user.target", "default.target"]
        );
        assert_eq!(section.required_by, vec!["other.service"]);
    }

    #[test]
    fn parse_install_section_accumulates_and_resets() {
        let unit =
            "[Install]\nWantedBy=a.target\nWantedBy=b.target\nRequiredBy=c.target\nRequiredBy=\n";
        let section = parse_install_section(unit);
        assert_eq!(section.wanted_by, vec!["a.target", "b.target"]);
        assert!(section.required_by.is_empty());
    }

    #[test]
    fn parse_install_section_ignores_other_sections_and_comments() {
        let unit = "[Service]\nWantedBy=not-install.target\n[Install]\n# comment\n; comment\nAlias=foo.service\nWantedBy=real.target\n[Unit]\nWantedBy=also-not.target\n";
        let section = parse_install_section(unit);
        assert_eq!(section.wanted_by, vec!["real.target"]);
        assert!(section.required_by.is_empty());
    }

    #[test]
    fn parse_install_section_missing_is_empty() {
        assert_eq!(
            parse_install_section("[Service]\nExecStart=/bin/true\n"),
            InstallSection::default()
        );
    }

    // install_systemd_units [Install] symlinks

    #[test]
    fn install_systemd_units_links_install_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let normal = tmp.path().join("normal");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("app.service"),
            "[Service]\nExecStart=/bin/true\n\n[Install]\nWantedBy=multi-user.target\nRequiredBy=critical.target\n",
        )
        .unwrap();
        fs::write(
            source.join("plain.service"),
            "[Service]\nExecStart=/bin/true\n",
        )
        .unwrap();

        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        install_systemd_units(&source, &normal, &HashMap::new(), &cfg).unwrap();

        let wants = normal.join("multi-user.target.wants/app.service");
        let requires = normal.join("critical.target.requires/app.service");
        assert!(wants.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(requires
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        // Relative link resolving to the installed unit next to the .wants dir.
        assert_eq!(
            fs::read_link(&wants).unwrap(),
            PathBuf::from("../app.service")
        );
        assert!(fs::read_to_string(&wants).unwrap().contains("ExecStart"));
        // Units without [Install] get no links.
        assert!(!normal
            .join("multi-user.target.wants/plain.service")
            .exists());
    }

    #[test]
    fn install_systemd_units_link_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let normal = tmp.path().join("normal");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("app.service"),
            "[Service]\nExecStart=/bin/true\n\n[Install]\nWantedBy=multi-user.target\n",
        )
        .unwrap();

        let cfg = crate::config::test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        install_systemd_units(&source, &normal, &HashMap::new(), &cfg).unwrap();
        install_systemd_units(&source, &normal, &HashMap::new(), &cfg).unwrap();

        let link = normal.join("multi-user.target.wants/app.service");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
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
