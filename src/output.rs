//! Stdout/stderr writer abstraction for QuadCD.
//!
//! Production code uses `Output::standard()` backed by real I/O handles;
//! tests construct an `Output` with in-memory buffers via `Output::new()`.

use std::cell::RefCell;
use std::io::{self, Write};

/// Wrapper around `RefMut<Box<dyn Write>>` that implements `Write` directly.
/// This lets call sites use `writeln!(output.err(), "...")` without `*` deref.
pub struct OutputGuard<'a>(std::cell::RefMut<'a, Box<dyn Write>>);

impl Write for OutputGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Holds the stdout and stderr writers for the application.
///
/// Uses `RefCell` for interior mutability so that `&Config` (which owns an
/// `Output`) can hand out mutable writer guards without requiring `&mut self`.
pub struct Output {
    stdout: RefCell<Box<dyn Write>>,
    stderr: RefCell<Box<dyn Write>>,
}

impl Output {
    /// Create an `Output` backed by real stdout/stderr.
    pub fn standard() -> Self {
        Self {
            stdout: RefCell::new(Box::new(io::stdout())),
            stderr: RefCell::new(Box::new(io::stderr())),
        }
    }

    /// Create an `Output` with custom writers (for tests).
    pub fn new(stdout: Box<dyn Write>, stderr: Box<dyn Write>) -> Self {
        Self {
            stdout: RefCell::new(stdout),
            stderr: RefCell::new(stderr),
        }
    }

    pub fn out(&self) -> OutputGuard<'_> {
        OutputGuard(self.stdout.borrow_mut())
    }

    pub fn err(&self) -> OutputGuard<'_> {
        OutputGuard(self.stderr.borrow_mut())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::rc::Rc;

    /// A shared buffer that can be cloned — one copy goes into `Output`, the
    /// other is kept by the test to inspect captured bytes.
    #[derive(Clone)]
    pub(crate) struct TestWriter(pub Rc<RefCell<Vec<u8>>>);

    impl TestWriter {
        pub fn new() -> Self {
            Self(Rc::new(RefCell::new(Vec::new())))
        }

        pub fn captured(&self) -> String {
            String::from_utf8_lossy(&self.0.borrow()).to_string()
        }
    }

    impl io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_captures_stdout() {
        let out_buf = TestWriter::new();
        let output = Output::new(Box::new(out_buf.clone()), Box::new(Vec::new()));
        writeln!(output.out(), "hello stdout").unwrap();
        assert_eq!(out_buf.captured(), "hello stdout\n");
    }

    #[test]
    fn output_captures_stderr() {
        let err_buf = TestWriter::new();
        let output = Output::new(Box::new(Vec::new()), Box::new(err_buf.clone()));
        writeln!(output.err(), "hello stderr").unwrap();
        assert_eq!(err_buf.captured(), "hello stderr\n");
    }

    #[test]
    fn output_multiple_writes() {
        let out_buf = TestWriter::new();
        let output = Output::new(Box::new(out_buf.clone()), Box::new(Vec::new()));
        writeln!(output.out(), "line 1").unwrap();
        writeln!(output.out(), "line 2").unwrap();
        assert_eq!(out_buf.captured(), "line 1\nline 2\n");
    }

    #[test]
    fn output_stdout_and_stderr_independent() {
        let out_buf = TestWriter::new();
        let err_buf = TestWriter::new();
        let output = Output::new(Box::new(out_buf.clone()), Box::new(err_buf.clone()));
        writeln!(output.out(), "out").unwrap();
        writeln!(output.err(), "err").unwrap();
        assert_eq!(out_buf.captured(), "out\n");
        assert_eq!(err_buf.captured(), "err\n");
    }

    #[test]
    fn output_standard_smoke() {
        // Just verify Output::standard() can be created and written to
        let output = Output::standard();
        // Write empty string to avoid polluting test output
        output.out().write_all(b"").unwrap();
        output.err().write_all(b"").unwrap();
    }

    #[test]
    fn output_guard_flush() {
        let out_buf = TestWriter::new();
        let output = Output::new(Box::new(out_buf.clone()), Box::new(Vec::new()));
        let mut guard = output.out();
        write!(guard, "data").unwrap();
        guard.flush().unwrap();
        drop(guard);
        assert_eq!(out_buf.captured(), "data");
    }
}
