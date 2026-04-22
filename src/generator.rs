use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use crate::output::Output;

/// Abstraction over the Podman systemd generator subprocess.
///
/// `GeneratorImpl` shells out to the podman generator binary; tests can
/// substitute a mock.
pub trait Generator {
    /// Run the generator with the given arguments and optional extra
    /// environment variables. Returns the exit code.
    fn run(&self, args: &[String], env: &[(&str, &str)], output: &Output) -> i32;
}

/// Generator implementation that shells out to the podman generator binary.
pub struct GeneratorImpl {
    pub path: PathBuf,
}

impl Generator for GeneratorImpl {
    fn run(&self, args: &[String], env: &[(&str, &str)], output: &Output) -> i32 {
        let mut cmd = Command::new(&self.path);
        cmd.args(args);
        for (key, val) in env {
            cmd.env(key, val);
        }
        match cmd.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(err) => {
                let _ = writeln!(output.err(), "Failed to run podman generator: {err}");
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_binary(candidates: &[&str]) -> PathBuf {
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("none of {candidates:?} found on this system"))
    }

    #[test]
    fn run_returns_zero_for_true() {
        let gen = GeneratorImpl {
            path: find_binary(&["/bin/true", "/usr/bin/true"]),
        };
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        assert_eq!(gen.run(&[], &[], &output), 0);
    }

    #[test]
    fn run_returns_one_for_false() {
        let gen = GeneratorImpl {
            path: find_binary(&["/bin/false", "/usr/bin/false"]),
        };
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        assert_eq!(gen.run(&[], &[], &output), 1);
    }

    #[test]
    fn run_returns_one_for_missing_binary() {
        let gen = GeneratorImpl {
            path: PathBuf::from("/no/such/binary"),
        };
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        assert_eq!(gen.run(&[], &[], &output), 1);
    }
}
