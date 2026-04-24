use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::cli::{
    parse_cli, GenerateInvocation, ParsedCommand, SyncInvocation, SYNC_USAGE, TOP_LEVEL_USAGE,
};
use crate::{dryrun, install, sync};

use super::{install_signal_handlers, Generator, GeneratorImpl, SHUTDOWN, VERSION};

/// Application context holding configuration and injected dependencies.
///
/// Created once in `main()` (or directly in tests) and used throughout a single
/// invocation.
pub struct App<'a> {
    pub cfg: crate::Config,
    vcs: Option<&'a dyn sync::Vcs>,
    systemd: Option<&'a dyn sync::SystemdTrait>,
    image_puller: Option<&'a dyn sync::ImagePuller>,
    generator: Option<&'a dyn Generator>,
}

impl<'a> App<'a> {
    pub fn new(cfg: crate::Config) -> Self {
        Self {
            cfg,
            vcs: None,
            systemd: None,
            image_puller: None,
            generator: None,
        }
    }

    /// Create an `App` with injected dependencies (for tests).
    pub fn new_with_deps(
        cfg: crate::Config,
        vcs: &'a dyn sync::Vcs,
        systemd: &'a dyn sync::SystemdTrait,
        image_puller: &'a dyn sync::ImagePuller,
        generator: &'a dyn Generator,
    ) -> Self {
        Self {
            cfg,
            vcs: Some(vcs),
            systemd: Some(systemd),
            image_puller: Some(image_puller),
            generator: Some(generator),
        }
    }

    /// Main entry point. Returns an exit code.
    pub fn run(&mut self, args: &[String]) -> i32 {
        match parse_cli(args, self.cfg.systemd_scope.as_deref()) {
            Ok(ParsedCommand::Help) => {
                self.print_usage();
                0
            }
            Ok(ParsedCommand::Version) => {
                let _ = writeln!(self.cfg.output.out(), "quadcd version {VERSION}");
                0
            }
            Ok(ParsedCommand::Generate(invocation)) => self.run_generate(invocation),
            Ok(ParsedCommand::Sync(invocation)) => self.run_sync(invocation),
            Err(err) => {
                err.emit(&self.cfg.output);
                1
            }
        }
    }

    fn print_usage(&self) {
        let _ = writeln!(self.cfg.output.err(), "{TOP_LEVEL_USAGE}");
    }

    /// Run the systemd generator flow.
    fn run_generate(&mut self, invocation: GenerateInvocation) -> i32 {
        if invocation.show_help {
            let _ = writeln!(
                self.cfg.output.err(),
                "Usage: {} generate [-v] [-no-kmsg-log] [-user] [-dryrun] [-version] normal-dir [early-dir] [late-dir]",
                invocation.program
            );
            return 0;
        }

        // Apply parsed flags to reconfigure mode-dependent fields.
        self.cfg
            .apply_flags(invocation.force_user, invocation.verbose, false);

        // QUADLET_UNIT_DIRS overrides the output directory for installed files.
        let quadlet_unit_dirs = self.cfg.quadlet_unit_dirs.clone();

        // Build the generator: use injected mock or real binary.
        let real_gen;
        let gen: &dyn Generator = match self.generator {
            Some(g) => g,
            None => {
                real_gen = GeneratorImpl {
                    path: self.cfg.podman_generator.clone(),
                };
                &real_gen
            }
        };

        // In dry-run mode, install into a temporary directory and run (not exec)
        // the Podman generator so the temp dir is cleaned up afterwards.
        if invocation.dryrun {
            return dryrun::DryRunner {
                cfg: &self.cfg,
                original_args: &invocation.original_args,
                generator: gen,
            }
            .run();
        }

        // Enumerate all source subdirectories in data_dir.
        let source_dirs = self.cfg.effective_source_dirs();

        let has_sources = source_dirs.iter().any(|(d, _)| d.exists());

        if !has_sources {
            if invocation.verbose {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] No source directories found in {}",
                    self.cfg.data_dir.display()
                );
            }
            if !self.cfg.podman_generator.exists() {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Podman generator not found, skipping: {}",
                    self.cfg.podman_generator.display()
                );
                return 0;
            }
            return gen.run(&invocation.original_args, &[], &self.cfg.output);
        }

        // positional is non-empty: the early return above rejects !dryrun with
        // no positional args, and dryrun returns before reaching this point.
        let normal_dir = PathBuf::from(
            invocation
                .positional
                .first()
                .expect("positional args non-empty: ensured by early return above"),
        );

        // If the Podman generator is not installed (e.g. podman not present),
        // warn and exit successfully — QuadCD's own file installation is done.
        if !self.cfg.podman_generator.exists() {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Podman generator not found, skipping: {}",
                self.cfg.podman_generator.display()
            );
            return 0;
        }

        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&self.cfg.podman_generator) {
                if meta.permissions().mode() & 0o111 == 0 {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "Podman generator not found or not executable: {}",
                        self.cfg.podman_generator.display()
                    );
                    return 1;
                }
            }
        }

        // Quadlet files go into a quadcd/ subdirectory of normal-dir (or a
        // QUADLET_UNIT_DIRS override).  The podman generator is then pointed at
        // this directory via the QUADLET_UNIT_DIRS env var so it discovers them.
        let quadlet_dir = quadlet_unit_dirs
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| normal_dir.join("quadcd"));

        if let Err(e) = fs::create_dir_all(&quadlet_dir) {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Failed to create quadlet dir {}: {e}",
                quadlet_dir.display()
            );
            return 1;
        }

        if invocation.verbose {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Normal dir: {}",
                normal_dir.display()
            );
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Quadlet dir: {}",
                quadlet_dir.display()
            );
        }

        install::warn_duplicate_units(&source_dirs, &self.cfg);

        // Install processed Quadlet and systemd unit files from all source dirs.
        // (systemd clears the normal-dir tree before calling generators, so the
        // quadlet subdirectory is always empty at this point.)
        for (source_dir, env_vars) in &source_dirs {
            if source_dir.exists() {
                if invocation.verbose {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Installing from {}",
                        source_dir.display()
                    );
                }
                if let Err(e) =
                    install::install_quadlet_files(source_dir, &quadlet_dir, env_vars, &self.cfg)
                {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Error installing Quadlet files: {e}"
                    );
                    return 1;
                }
                if let Err(e) =
                    install::install_systemd_units(source_dir, &normal_dir, env_vars, &self.cfg)
                {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "[quadcd] Error installing systemd units: {e}"
                    );
                    return 1;
                }
            }
        }

        // Symlink drop-in directories from the standard Quadlet directory
        // so the Podman generator discovers global/per-unit overrides.
        if let Some(ref dropins_dir) = self.cfg.quadlet_dropins_dir {
            if let Err(e) = install::symlink_dropins(dropins_dir, &quadlet_dir, &self.cfg) {
                let _ = writeln!(
                    self.cfg.output.err(),
                    "[quadcd] Warning: failed to symlink drop-ins: {e}"
                );
            }
        }

        if invocation.verbose {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Invoking Podman generator: {}",
                self.cfg.podman_generator.display()
            );
        }

        // Run the Podman generator with QUADLET_UNIT_DIRS pointing at our
        // quadlet subdirectory so it discovers the installed Quadlet files.
        let quadlet_dir_str = quadlet_dir.to_string_lossy();
        let rc = gen.run(
            &invocation.original_args,
            &[("QUADLET_UNIT_DIRS", quadlet_dir_str.as_ref())],
            &self.cfg.output,
        );

        // The podman generator adds its own SourcePath= (pointing at the
        // quadlet_dir copy).  Our install step already injected the real
        // SourcePath first, so dropping duplicates keeps the correct value.
        if let Err(e) = install::clean_generated_source_paths(&normal_dir) {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Warning: failed to clean SourcePath: {e}"
            );
        }

        rc
    }

    /// Run the sync subcommand.
    fn run_sync(&mut self, invocation: SyncInvocation) -> i32 {
        if invocation.show_help {
            let _ = writeln!(self.cfg.output.err(), "{SYNC_USAGE}");
            return 0;
        }

        // Apply flags to reconfigure mode.
        self.cfg
            .apply_flags(invocation.force_user, invocation.verbose, invocation.force);

        let default_vcs;
        let vcs: &dyn sync::Vcs = match self.vcs {
            Some(v) => v,
            None => {
                default_vcs = sync::GitVcs::with_command(
                    self.cfg.git_command.as_deref(),
                    self.cfg.git_timeout,
                )
                .known_hosts(self.cfg.data_dir.join(".known_hosts"))
                .accept_new_host_keys(invocation.accept_new_host_keys)
                .interactive(invocation.interactive);
                &default_vcs
            }
        };
        if let Err(e) = vcs.check() {
            let _ = writeln!(self.cfg.output.err(), "Error: {e}");
            return 1;
        }

        let cd_config = match self.cfg.cd_config {
            Some(ref c) => c.clone(),
            None => {
                if let Some(ref path) = self.cfg.config_path {
                    let _ = writeln!(
                        self.cfg.output.err(),
                        "Error: config file '{}' could not be loaded (see warning above)",
                        path.display()
                    );
                } else {
                    let _ = writeln!(self.cfg.output.err(), "Error: no config file found");
                    if self.cfg.is_user_mode {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "Create ~/.config/quadcd.toml or set QUADCD_CONFIG"
                        );
                    } else {
                        let _ = writeln!(
                            self.cfg.output.err(),
                            "Create /etc/quadcd.toml or set QUADCD_CONFIG"
                        );
                    }
                }
                return 1;
            }
        };

        if cd_config.repositories.is_empty() {
            let _ = writeln!(self.cfg.output.err(), "Error: no repositories configured");
            return 1;
        }

        if let Err(e) = fs::create_dir_all(&self.cfg.data_dir) {
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Failed to create data dir {}: {e}",
                self.cfg.data_dir.display()
            );
            return 1;
        }

        if invocation.verbose {
            let mode = if self.cfg.is_user_mode {
                "user"
            } else {
                "system"
            };
            let _ = writeln!(self.cfg.output.err(), "[quadcd] Running in {mode} mode");
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] Data dir: {}",
                self.cfg.data_dir.display()
            );
            let _ = writeln!(
                self.cfg.output.err(),
                "[quadcd] {} repository(ies) configured",
                cd_config.repositories.len()
            );
        }

        let default_systemd;
        let systemd: &dyn sync::SystemdTrait = match self.systemd {
            Some(s) => s,
            None => {
                default_systemd = sync::Systemd::new();
                &default_systemd
            }
        };

        let default_podman;
        let image_puller: &dyn sync::ImagePuller = match self.image_puller {
            Some(p) => p,
            None => {
                default_podman = sync::Podman::new();
                &default_podman
            }
        };

        let runner = sync::SyncRunner::new(&self.cfg, vcs, systemd, image_puller)
            .sync_only(invocation.sync_only);

        if invocation.service {
            install_signal_handlers();
            runner.run_service(cd_config, &SHUTDOWN);
            0
        } else {
            let _lock = match install::acquire_sync_lock(&self.cfg.data_dir) {
                Ok(f) => f,
                Err(e) => {
                    let _ = writeln!(self.cfg.output.err(), "Error: {e}");
                    return 1;
                }
            };
            let failures = runner.run_once(&cd_config);
            if failures > 0 {
                1
            } else {
                0
            }
        }
    }
}
