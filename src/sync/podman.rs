//! Podman implementation for container image pre-pulling.

use std::{io::Write, time::Duration};

use subprocess::{Exec, Redirection};

use crate::config::Config;

use super::image::{ImagePuller, ImageRef};

/// Podman implementation that shells out to the `podman` binary.
pub struct Podman {
    cmd: String,
    env: Vec<(String, String)>,
}

impl Default for Podman {
    fn default() -> Self {
        Self::new()
    }
}

impl Podman {
    /// Create a `Podman` using the default `podman` binary.
    pub fn new() -> Self {
        Self {
            cmd: "podman".to_string(),
            env: Vec::new(),
        }
    }

    /// Set custom command path.
    pub fn command(mut self, cmd: &str) -> Self {
        self.cmd = cmd.to_string();
        self
    }

    /// Add an environment variable to all spawned commands.
    pub fn env(mut self, key: &str, val: &str) -> Self {
        self.env.push((key.to_string(), val.to_string()));
        self
    }

    fn exec(&self) -> Exec {
        let mut e = Exec::cmd(&self.cmd).stdin(Redirection::Null);
        for (k, v) in &self.env {
            e = e.env(k, v);
        }
        e
    }
}

impl ImagePuller for Podman {
    fn pull(&self, image: &ImageRef, cfg: &Config) {
        let mut args: Vec<&str> = vec!["pull"];

        if let Some(auth_file) = &image.auth_file {
            args.push("--authfile");
            args.push(auth_file);
        }
        if let Some(tls_verify) = image.tls_verify {
            let flag = if tls_verify {
                "--tls-verify=true"
            } else {
                "--tls-verify=false"
            };
            args.push(flag);
        }

        args.push(&image.image);

        if cfg.verbose {
            let _ = writeln!(
                cfg.output.err(),
                "[quadcd] Pre-pulling image: {}",
                image.image
            );
        }

        let mut job = match self
            .exec()
            .args(&args)
            .stdout(Redirection::Null)
            .stderr(Redirection::Pipe)
            .start()
        {
            Ok(job) => job,
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: failed to run podman pull: {e}"
                );
                return;
            }
        };

        let comm = match job.communicate() {
            Ok(comm) => comm,
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: podman pull {}: {e}",
                    image.image
                );
                return;
            }
        };

        let io_result = comm.limit_time(cfg.podman_pull_timeout).read();

        match io_result {
            Ok((_stdout, stderr)) => match job.wait() {
                Ok(status) if !status.success() => {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Warning: failed to pull image {}: {}",
                        image.image,
                        String::from_utf8_lossy(&stderr).trim()
                    );
                }
                Ok(_) => {
                    if cfg.verbose {
                        let _ = writeln!(
                            cfg.output.err(),
                            "[quadcd] Successfully pulled image: {}",
                            image.image
                        );
                    }
                }
                Err(e) => {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Warning: podman pull {} wait failed: {e}",
                        image.image
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                job.terminate().ok();
                job.wait_timeout(Duration::from_secs(5)).ok();
                job.kill().ok();

                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Timed out after {}s while pulling image {}: {e}",
                    cfg.podman_pull_timeout.as_secs(),
                    image.image,
                );
            }
            Err(e) => {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] Warning: podman pull {}: {e}",
                    image.image
                );
            }
        }
    }
}
