//! Shared subprocess helpers.

use std::io::Write;

use subprocess::{Exec, ExitStatus};

use crate::output::Output as AppOutput;

/// Run `exec` and, when `sub_out` is `Some`, capture stdout/stderr and write
/// them between `[quadcd] --- begin/end {label} ---` markers.
/// When `sub_out` is `None`, stdout/stderr are inherited from the parent.
pub(crate) fn run_with_markers(
    exec: Exec,
    label: &str,
    sub_out: Option<&AppOutput>,
) -> Result<ExitStatus, std::io::Error> {
    if let Some(out) = sub_out {
        let capture = exec.capture()?;
        let writes = || -> std::io::Result<()> {
            if !capture.stdout.is_empty() {
                writeln!(out.out(), "[quadcd] --- begin {label} ---")?;
                out.out().write_all(&capture.stdout)?;
                writeln!(out.out(), "[quadcd] --- end {label} ---")?;
            }
            writeln!(out.err(), "[quadcd] --- begin {label} ---")?;
            if !capture.stderr.is_empty() {
                out.err().write_all(&capture.stderr)?;
            }
            writeln!(out.err(), "[quadcd] --- end {label} ---")
        };
        if let Err(e) = writes() {
            eprintln!("[quadcd] warning: failed writing subprocess output: {e}");
        }
        Ok(capture.exit_status)
    } else {
        exec.join()
    }
}
