//! Shared test infrastructure for integration tests.

use quadcd::sync::{ImagePuller, ImageRef, SystemdTrait, Vcs};
use quadcd::{App, Config, Generator};
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Test writer (in-memory stdout/stderr capture)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TestWriter(Rc<RefCell<Vec<u8>>>);

impl TestWriter {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Vec::new())))
    }
    #[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct NoopVcs;

impl Vcs for NoopVcs {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn clone_repo(&self, _url: &str, _branch: Option<&str>, _target: &Path) -> Result<(), String> {
        Ok(())
    }
    fn head_sha(&self, _repo_dir: &Path) -> Option<String> {
        None
    }
    fn changed_files(&self, _repo_dir: &Path, _old: &str, _new: &str) -> Vec<String> {
        Vec::new()
    }
    fn remote_url(&self, _repo_dir: &Path) -> Result<String, String> {
        Ok(String::new())
    }
    fn set_remote_url(&self, _repo_dir: &Path, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn fetch(&self, _repo_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    fn reset_hard(&self, _repo_dir: &Path, _branch: &str) -> Result<(), String> {
        Ok(())
    }
    fn pull_ff_only(&self, _repo_dir: &Path, _branch: &str) -> Result<(), String> {
        Ok(())
    }
    fn default_branch(&self, _repo_dir: &Path) -> String {
        "main".to_string()
    }
}

#[allow(dead_code)]
pub struct NoopSystemd;

impl SystemdTrait for NoopSystemd {
    fn daemon_reload(&self, _cfg: &Config) {}
    fn restart(&self, _units: &[String], _cfg: &Config) {}
    fn start(&self, _units: &[String], _cfg: &Config) {}
    fn is_enabled(&self, _unit: &str, _cfg: &Config) -> String {
        "disabled".into()
    }
    fn is_active(&self, _unit: &str, _cfg: &Config) -> bool {
        false
    }
    fn list_units_matching(&self, _pattern: &str, _cfg: &Config) -> Vec<String> {
        vec![]
    }
}

#[allow(dead_code)]
pub struct NoopImagePuller;

impl ImagePuller for NoopImagePuller {
    fn pull(&self, _image: &ImageRef, _cfg: &Config) {}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn true_binary() -> PathBuf {
    for p in &["/bin/true", "/usr/bin/true"] {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }
    panic!("neither /bin/true nor /usr/bin/true found on this system");
}

pub fn test_config(out: &TestWriter, err: &TestWriter) -> Config {
    Config::for_testing(Box::new(out.clone()), Box::new(err.clone()))
}

#[allow(dead_code)]
pub fn make_app<'a>(
    out_buf: &TestWriter,
    err_buf: &TestWriter,
    vcs: &'a NoopVcs,
    systemd: &'a NoopSystemd,
    gen: &'a dyn Generator,
) -> App<'a> {
    let cfg = test_config(out_buf, err_buf);
    App::new_with_deps(cfg, vcs, systemd, &NoopImagePuller, gen)
}
