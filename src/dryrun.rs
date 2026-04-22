//! Dry-run support for QuadCD.
//!
//! In dry-run mode, source files are previewed (with env substitution applied),
//! installed into a temporary directory, and the Podman generator is invoked as
//! a subprocess with `QUADLET_UNIT_DIRS` pointing at the quadcd subdirectory
//! inside the temp dir. The generated output is printed and the temp dir is
//! automatically cleaned up.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::cd_config::CDConfig;
use crate::config::Config;
use crate::install::{envsubst, find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};
use crate::output::Output;
use crate::Generator;

/// Print the contents of all source Quadlet and systemd unit files
/// with environment variable substitution applied.
fn preview(source_dir: &Path, env_vars: &HashMap<String, String>, cfg: &Config) {
    if !source_dir.exists() {
        if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Source directory {} does not exist",
                source_dir.display()
            );
        }
        return;
    }

    let dir_name = source_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| source_dir.display().to_string());

    let quadlet_files = find_files(source_dir, QUADLET_EXTENSIONS);
    if !quadlet_files.is_empty() {
        if cfg.verbose {
            let _ = writeln!(cfg.output.err(), "[quadcd] Quadlet files from {dir_name}/:");
        }
        for file in &quadlet_files {
            let name = file.file_name().unwrap().to_string_lossy();
            let content = match fs::read_to_string(file) {
                Ok(c) => c,
                Err(e) => {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Warning: could not read {}: {e}",
                        file.display()
                    );
                    continue;
                }
            };
            let _ = writeln!(cfg.output.out(), "### {dir_name}/{name} ###");
            let content = envsubst(&content, env_vars);
            let _ = writeln!(cfg.output.out(), "{content}");
        }
    }

    let systemd_files = find_files(source_dir, SYSTEMD_EXTENSIONS);
    if !systemd_files.is_empty() {
        if cfg.verbose {
            let _ = writeln!(cfg.output.err(), "[quadcd] Systemd units from {dir_name}/:");
        }
        for file in &systemd_files {
            let name = file.file_name().unwrap().to_string_lossy();
            let content = match fs::read_to_string(file) {
                Ok(c) => c,
                Err(e) => {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Warning: could not read {}: {e}",
                        file.display()
                    );
                    continue;
                }
            };
            let _ = writeln!(cfg.output.out(), "### {dir_name}/{name} ###");
            let content = envsubst(&content, env_vars);
            let _ = writeln!(cfg.output.out(), "{content}");
        }
    }
}

/// Show CD config information: which repos would be synced.
fn preview_cd_config(cd_config: &CDConfig, data_dir: &Path, output: &Output) {
    let _ = writeln!(output.err(), "[quadcd] Configured repositories:");
    for (name, repo) in &cd_config.repositories {
        let repo_dir = data_dir.join(name);
        let status = if repo_dir.join(".git").exists() {
            "cloned"
        } else {
            "not yet cloned"
        };
        let branch = repo.branch.as_deref().unwrap_or("(default)");
        let interval = repo.interval.as_deref().unwrap_or("manual");
        let _ = writeln!(
            output.err(),
            "[quadcd]   {name}: {} (branch: {branch}, interval: {interval}, status: {status})",
            repo.url
        );
    }
}

/// Holds shared context for a dry-run invocation.
pub struct DryRunner<'a> {
    pub(crate) cfg: &'a Config,
    pub(crate) original_args: &'a [String],
    pub(crate) generator: &'a dyn Generator,
}

#[cfg(feature = "test-support")]
impl<'a> DryRunner<'a> {
    pub fn new_for_test(
        cfg: &'a Config,
        original_args: &'a [String],
        generator: &'a dyn Generator,
    ) -> Self {
        Self {
            cfg,
            original_args,
            generator,
        }
    }
}

impl<'a> DryRunner<'a> {
    /// Execute a full dry-run: preview source files, install into a temp dir,
    /// run the Podman generator, and print generated output.
    ///
    /// Returns 0 on success, 1 on failure.
    pub fn run(&self) -> i32 {
        let verbose = self.cfg.verbose;

        // Show CD config info if available
        if let Some(ref cd_config) = self.cfg.cd_config {
            preview_cd_config(cd_config, &self.cfg.data_dir, &self.cfg.output);
        }

        // Preview files from all source subdirectories.
        let source_dirs = self.cfg.effective_source_dirs();
        if source_dirs.is_empty() {
            if verbose {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] No source directories found in {}",
                    self.cfg.data_dir.display()
                );
            }
        } else {
            for (dir, env_vars) in &source_dirs {
                preview(dir, env_vars, self.cfg);
            }
        }

        let tmp_dir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Failed to create temp directory: {e}"
                );
                return 1;
            }
        };

        let tmp_path = tmp_dir.path();
        let normal_dir = tmp_path.join("normal");
        let quadlet_dir = normal_dir.join("quadcd");
        if let Err(e) = fs::create_dir_all(&quadlet_dir) {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Failed to create temp quadlet dir {}: {e}",
                quadlet_dir.display()
            );
            return 1;
        }

        if verbose {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Dry-run temp dir: {}",
                tmp_path.display()
            );
        }

        crate::install::warn_duplicate_units(&source_dirs, self.cfg);

        // Install from all source subdirectories
        for (dir, env_vars) in &source_dirs {
            if dir.exists() {
                if let Err(e) =
                    crate::install::install_quadlet_files(dir, &quadlet_dir, env_vars, self.cfg)
                {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Error installing Quadlet files: {e}"
                    );
                }
                if let Err(e) =
                    crate::install::install_systemd_units(dir, &normal_dir, env_vars, self.cfg)
                {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Error installing systemd units: {e}"
                    );
                }
            }
        }

        // Symlink drop-in directories so the generator discovers them.
        if let Some(ref dropins_dir) = self.cfg.quadlet_dropins_dir {
            if let Err(e) = crate::install::symlink_dropins(dropins_dir, &quadlet_dir, self.cfg) {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: failed to symlink drop-ins: {e}"
                );
            }
        }

        if !self.cfg.podman_generator.exists() {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Podman generator not found, skipping: {}",
                self.cfg.podman_generator.display()
            );
        } else {
            if verbose {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Invoking Podman generator (dry-run): {}",
                    self.cfg.podman_generator.display()
                );
            }

            // Run the generator with QUADLET_UNIT_DIRS pointing at the quadcd
            // subdirectory so it discovers the installed Quadlet files.
            // NOTE: The podman generator prints to stdout in dryrun (no output
            // dirs), so quadlet-generated units may show duplicate SourcePath=
            // lines (ours + the generator's).  This is cosmetic; real units
            // installed via run_generate are cleaned by clean_generated_source_paths.
            let quadlet_dir_str = quadlet_dir.to_string_lossy();
            self.generator.run(
                self.original_args,
                &[("QUADLET_UNIT_DIRS", quadlet_dir_str.as_ref())],
                &self.cfg.output,
            );

            if let Err(e) = crate::install::clean_generated_source_paths(&normal_dir) {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: failed to clean SourcePath: {e}"
                );
            }
        }

        // Print generated output from the normal-dir inside the temp directory.
        if verbose {
            let _ = writeln!(self.cfg.output.err(), "[quadcd] Generated units:");
        }
        if let Ok(entries) = fs::read_dir(&normal_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = path.file_name().unwrap().to_string_lossy();
                    let _ = writeln!(self.cfg.output.out(), "### {name} (generated) ###");
                    let content = match fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = writeln!(
                                self.cfg.output.err(),
                                "[quadcd] Warning: could not read generated file {}: {e}",
                                path.display()
                            );
                            continue;
                        }
                    };
                    let _ = writeln!(self.cfg.output.out(), "{content}");
                }
            }
        }

        // tmp_dir is cleaned up on drop
        0
    }
}
