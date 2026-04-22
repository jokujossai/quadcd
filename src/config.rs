//! Runtime configuration for QuadCD.
//!
//! Holds all resolved paths, environment overrides, and optional CD config
//! in a single `Config` struct. Production code constructs it via
//! `Config::from_env()`; tests construct it directly.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cd_config::CDConfig;
use crate::output::Output;

/// Holds all resolved paths, environment variables, and optional CD config
/// needed for a QuadCD run.
pub struct Config {
    // -- Mode & flags --
    pub is_user_mode: bool,
    pub verbose: bool,
    pub force: bool,

    // -- I/O --
    pub output: Output,
    /// Optional sink for subprocess stdout/stderr. When `Some`, subprocesses
    /// have their output captured and forwarded here with begin/end markers;
    /// otherwise they inherit the parent's I/O handles.
    pub subprocess_output: Option<Output>,

    // -- Resolved paths --
    pub home: String,
    pub data_dir: PathBuf,
    pub source_dir: PathBuf,
    pub podman_generator: PathBuf,
    pub config_path: Option<PathBuf>,

    // -- Raw env overrides (kept for runtime logic) --
    pub quadcd_unit_dirs: Option<String>,
    pub quadlet_unit_dirs: Option<String>,
    pub quadlet_dropins_dir: Option<PathBuf>,
    pub git_command: Option<String>,
    pub git_timeout: Duration,
    pub podman_pull_timeout: Duration,

    // -- Base env vars from data-dir .env file --
    pub env_vars: HashMap<String, String>,

    // -- CD config (optional — only present when quadcd.toml exists) --
    pub cd_config: Option<CDConfig>,

    // -- Systemd scope (from SYSTEMD_SCOPE env var) --
    pub systemd_scope: Option<String>,

    // -- Internal --
    is_root: bool,
    podman_generator_path: Option<String>,
    quadcd_config: Option<String>,
    quadlet_dropins_unit_dirs: Option<String>,
}

impl Config {
    /// Build configuration from real process environment variables.
    ///
    /// CLI flags (`verbose`, `force_user`) default to false; call
    /// `apply_flags()` after parsing args to reconfigure mode-dependent fields.
    pub fn from_env() -> Self {
        let home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let quadcd_config = env_var_non_empty("QUADCD_CONFIG");
        let quadcd_unit_dirs = env_var_non_empty("QUADCD_UNIT_DIRS");
        let quadlet_unit_dirs = env_var_non_empty("QUADLET_UNIT_DIRS");
        let quadlet_dropins_unit_dirs = env_var_non_empty("QUADLET_DROPINS_UNIT_DIRS");
        let podman_generator_path = env_var_non_empty("PODMAN_GENERATOR_PATH");
        let git_command = env_var_non_empty("GIT_COMMAND");
        let git_timeout = parse_git_timeout(env_var_non_empty("GIT_TIMEOUT").as_deref());
        let podman_pull_timeout =
            parse_podman_pull_timeout(env_var_non_empty("PODMAN_PULL_TIMEOUT").as_deref());
        let systemd_scope = env_var_non_empty("SYSTEMD_SCOPE");

        let is_root = is_root();
        let is_user_mode = detect_mode(false, is_root, systemd_scope.as_deref());
        let data_dir = data_dir_from(is_user_mode, &home);
        let source_dir = resolve_source_dir(quadcd_unit_dirs.as_deref(), &data_dir);

        let podman_generator = resolve_generator(is_user_mode, podman_generator_path.as_deref());

        let output = Output::standard();
        let env_vars = load_env_file(&data_dir, false, &output);

        let quadlet_dropins_dir =
            resolve_dropins_dir(quadlet_dropins_unit_dirs.as_deref(), is_user_mode, &home);

        let config_path = CDConfig::config_path(is_user_mode, &home, quadcd_config.as_deref());
        let cd_config = Self::try_load_cd_config(&config_path, &quadcd_config, &output);

        Config {
            is_user_mode,
            verbose: false,
            force: false,
            output,
            subprocess_output: None,
            home,
            data_dir,
            source_dir,
            podman_generator,
            config_path,
            quadcd_unit_dirs,
            quadlet_unit_dirs,
            quadlet_dropins_dir,
            git_command,
            git_timeout,
            podman_pull_timeout,
            env_vars,
            cd_config,
            systemd_scope,
            is_root,
            podman_generator_path,
            quadcd_config,
            quadlet_dropins_unit_dirs,
        }
    }

    /// Reconfigure mode-dependent fields after CLI flag parsing.
    pub fn apply_flags(&mut self, force_user: bool, verbose: bool, force: bool) {
        let is_user_mode = detect_mode(force_user, self.is_root, self.systemd_scope.as_deref());
        self.is_user_mode = is_user_mode;
        self.verbose = verbose;
        self.force = force;

        self.podman_generator =
            resolve_generator(is_user_mode, self.podman_generator_path.as_deref());

        self.data_dir = data_dir_from(is_user_mode, &self.home);

        self.source_dir = resolve_source_dir(self.quadcd_unit_dirs.as_deref(), &self.data_dir);

        self.quadlet_dropins_dir = resolve_dropins_dir(
            self.quadlet_dropins_unit_dirs.as_deref(),
            is_user_mode,
            &self.home,
        );

        if verbose {
            let mode_name = if is_user_mode { "user" } else { "system" };
            let _ = writeln!(self.output.err(), "[quadcd] Running in {mode_name} mode");
            let _ = writeln!(
                self.output.err(),
                "[quadcd] Source: {}",
                self.source_dir.display()
            );
        }

        self.env_vars = load_env_file(&self.data_dir, verbose, &self.output);

        self.config_path =
            CDConfig::config_path(is_user_mode, &self.home, self.quadcd_config.as_deref());
        self.cd_config =
            Self::try_load_cd_config(&self.config_path, &self.quadcd_config, &self.output);
    }

    /// Try to load the CD config, logging warnings on failure.
    fn try_load_cd_config(
        config_path: &Option<PathBuf>,
        quadcd_config: &Option<String>,
        output: &Output,
    ) -> Option<CDConfig> {
        if quadcd_config.is_some() && config_path.is_none() {
            let _ = writeln!(
                output.err(),
                "[quadcd] Warning: QUADCD_CONFIG='{}' but file does not exist",
                quadcd_config.as_ref().unwrap()
            );
        }

        config_path
            .as_ref()
            .and_then(|p| match CDConfig::load_from_path(p) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    let _ = writeln!(output.err(), "[quadcd] Warning: {e}");
                    None
                }
            })
    }

    /// Return the effective source directories for this run.
    ///
    /// When `quadcd_unit_dirs` is set, returns only the single overridden
    /// source dir. Otherwise, enumerates all subdirectories in data_dir.
    pub fn effective_source_dirs(&self) -> Vec<(PathBuf, HashMap<String, String>)> {
        if self.quadcd_unit_dirs.is_some() {
            let dir_env = load_env_file(&self.source_dir, self.verbose, &self.output);
            let effective = if dir_env.is_empty() {
                self.env_vars.clone()
            } else {
                merge_env_vars(&self.env_vars, &dir_env)
            };
            vec![(self.source_dir.clone(), effective)]
        } else {
            self.source_dirs()
        }
    }

    /// Return all subdirectories in the data directory that can serve as source
    /// directories (local/ plus any git repo dirs).
    fn source_dirs(&self) -> Vec<(PathBuf, HashMap<String, String>)> {
        let mut dirs = Vec::new();

        if !self.data_dir.exists() {
            return dirs;
        }

        let mut entries: Vec<PathBuf> = match fs::read_dir(&self.data_dir) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect(),
            Err(_) => return dirs,
        };
        entries.sort();

        for entry in entries {
            let dir_env = load_env_file(&entry, self.verbose, &self.output);
            let effective = if dir_env.is_empty() {
                self.env_vars.clone()
            } else {
                let merged = merge_env_vars(&self.env_vars, &dir_env);
                if self.verbose {
                    let dir_name = entry
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| entry.display().to_string());
                    let mut var_names: Vec<&str> = merged.keys().map(|s| s.as_str()).collect();
                    var_names.sort_unstable();
                    let _ = writeln!(
                        self.output.err(),
                        "[quadcd] Effective variables for {dir_name}/: {}",
                        var_names.join(" ")
                    );
                }
                merged
            };
            dirs.push((entry, effective));
        }

        dirs
    }
}

/// Return the base data directory.
///
/// - User mode: `~/.local/share/quadcd/`
/// - System mode: `/var/lib/quadcd/`
pub fn data_dir_from(is_user_mode: bool, home: &str) -> PathBuf {
    if is_user_mode {
        PathBuf::from(format!("{home}/.local/share/quadcd"))
    } else {
        PathBuf::from("/var/lib/quadcd")
    }
}

/// Default git operation timeout (5 minutes).
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Parse the `GIT_TIMEOUT` environment variable into a `Duration`.
///
/// Accepts a plain integer (seconds) or `None` (returns the default of 300s).
fn parse_git_timeout(value: Option<&str>) -> Duration {
    match value {
        Some(s) => s
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_GIT_TIMEOUT),
        None => DEFAULT_GIT_TIMEOUT,
    }
}

/// Default podman pull timeout (60 seconds).
const DEFAULT_PODMAN_PULL_TIMEOUT: Duration = Duration::from_secs(60);

/// Parse the `PODMAN_PULL_TIMEOUT` environment variable into a `Duration`.
///
/// Accepts a plain integer (seconds) or `None` (returns the default of 60s).
fn parse_podman_pull_timeout(value: Option<&str>) -> Duration {
    match value {
        Some(s) => s
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_PODMAN_PULL_TIMEOUT),
        None => DEFAULT_PODMAN_PULL_TIMEOUT,
    }
}

/// Read an environment variable, treating empty values as unset.
fn env_var_non_empty(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolve the podman generator binary path.
fn resolve_generator(is_user_mode: bool, podman_generator_path: Option<&str>) -> PathBuf {
    match podman_generator_path.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            if is_user_mode {
                PathBuf::from("/usr/lib/systemd/user-generators/podman-user-generator")
            } else {
                PathBuf::from("/usr/lib/systemd/system-generators/podman-system-generator")
            }
        }
    }
}

/// Resolve the directory to scan for Quadlet drop-in directories.
///
/// When `QUADLET_DROPINS_UNIT_DIRS` is set, its value is used as-is.
/// Otherwise, the standard Podman Quadlet directory is used:
/// - User mode: `~/.config/containers/systemd/`
/// - System mode: `/etc/containers/systemd/`
///
/// Returns `None` if the resolved directory does not exist.
fn resolve_dropins_dir(
    quadlet_dropins_unit_dirs: Option<&str>,
    is_user_mode: bool,
    home: &str,
) -> Option<PathBuf> {
    let dir = match quadlet_dropins_unit_dirs.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            if is_user_mode {
                PathBuf::from(format!("{home}/.config/containers/systemd"))
            } else {
                PathBuf::from("/etc/containers/systemd")
            }
        }
    };
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Resolve the source directory from an optional `QUADCD_UNIT_DIRS` override.
///
/// When the environment variable is set, its value is used as-is.
/// Otherwise, falls back to `data_dir`.
fn resolve_source_dir(quadcd_unit_dirs: Option<&str>, data_dir: &Path) -> PathBuf {
    quadcd_unit_dirs
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.to_path_buf())
}

/// Determine whether QuadCD should run in user mode.
///
/// The `is_root` parameter controls the UID-based fallback, allowing tests
/// to exercise both code paths without requiring real root privileges.
fn detect_mode(force_user: bool, is_root: bool, systemd_scope: Option<&str>) -> bool {
    if force_user {
        return true;
    }

    if let Ok(path) = env::current_exe().and_then(|p| p.canonicalize()) {
        let path_str = path.to_string_lossy();
        if path_str.contains("/user-generators/") {
            return true;
        }
        if path_str.contains("/system-generators/") {
            return false;
        }
    }

    // SYSTEMD_SCOPE: "system" means system mode, anything else means user mode.
    if let Some(scope) = systemd_scope.filter(|s| !s.is_empty()) {
        return scope != "system";
    }

    // Fallback: non-root runs in user mode
    !is_root
}

/// Check whether the current process is running as root.
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Create a minimal `Config` for use in unit tests.
#[cfg(test)]
pub(crate) fn test_config(
    stdout: Box<dyn std::io::Write>,
    stderr: Box<dyn std::io::Write>,
) -> Config {
    Config::for_testing(stdout, stderr)
}

#[cfg(any(test, feature = "test-support"))]
impl Config {
    /// Create a `Config` with deterministic defaults, without reading
    /// environment variables or inspecting the real system.
    pub fn for_testing(stdout: Box<dyn std::io::Write>, stderr: Box<dyn std::io::Write>) -> Self {
        Self {
            is_user_mode: true,
            verbose: false,
            force: false,
            output: Output::new(stdout, stderr),
            subprocess_output: None,
            home: "/tmp/test-home".to_string(),
            data_dir: PathBuf::from("/tmp/test-data"),
            source_dir: PathBuf::from("/tmp/test-source"),
            podman_generator: PathBuf::from(
                ["/bin/true", "/usr/bin/true"]
                    .iter()
                    .find(|p| Path::new(p).exists())
                    .unwrap_or(&"/usr/bin/true"),
            ),
            config_path: None,
            quadcd_unit_dirs: None,
            quadlet_unit_dirs: None,
            quadlet_dropins_dir: None,
            git_command: None,
            git_timeout: Duration::from_secs(300),
            podman_pull_timeout: Duration::from_secs(60),
            env_vars: HashMap::new(),
            cd_config: None,
            systemd_scope: None,
            is_root: false,
            podman_generator_path: None,
            quadcd_config: None,
            quadlet_dropins_unit_dirs: None,
        }
    }

    /// Set the private `podman_generator_path` field.
    pub fn set_podman_generator_path(&mut self, path: Option<String>) {
        self.podman_generator_path = path;
    }
}

/// Load key-value pairs from a `.env` file in the given directory.
///
/// Uses `dotenvy` for parsing, which handles quoted values, comments,
/// `export` prefixes, and other `.env` conventions.
pub fn load_env_file(source_dir: &Path, verbose: bool, output: &Output) -> HashMap<String, String> {
    let env_file = source_dir.join(".env");

    let file = match fs::File::open(&env_file) {
        Ok(f) => f,
        Err(_) => return HashMap::new(),
    };

    if verbose {
        let _ = writeln!(
            output.err(),
            "[quadcd] Loading environment from {}",
            env_file.display()
        );
    }

    let vars: HashMap<String, String> = dotenvy::from_read_iter(file)
        .filter_map(|item| item.ok())
        .collect();

    if verbose {
        let var_names: Vec<&str> = vars.keys().map(|s| s.as_str()).collect();
        let _ = writeln!(
            output.err(),
            "[quadcd] Variables for substitution: {}",
            var_names.join(" ")
        );
    }

    vars
}

/// Merge two sets of environment variables.
///
/// Returns a new map containing all entries from `base`, with any
/// entries in `overrides` taking precedence for duplicate keys.
fn merge_env_vars(
    base: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = base.clone();
    merged.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;

    // data_dir_from

    #[rstest]
    #[case::user_mode(true, "/home/alice/.local/share/quadcd")]
    #[case::system_mode(false, "/var/lib/quadcd")]
    fn test_data_dir_from(#[case] user_mode: bool, #[case] expected: &str) {
        assert_eq!(
            data_dir_from(user_mode, "/home/alice"),
            PathBuf::from(expected)
        );
    }

    // load_env_file

    #[test]
    fn load_env_file_reads_dotenv() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".env"), "PORT=8080\nHOST=localhost").unwrap();
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        let vars = load_env_file(tmp.path(), false, &output);
        assert_eq!(vars["PORT"], "8080");
        assert_eq!(vars["HOST"], "localhost");
    }

    #[test]
    fn load_env_file_missing_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        let vars = load_env_file(tmp.path(), false, &output);
        assert!(vars.is_empty());
    }

    #[test]
    fn load_env_file_strips_quotes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".env"),
            "DOUBLE=\"hello world\"\nSINGLE='foo bar'\nPLAIN=baz",
        )
        .unwrap();
        let output = Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        let vars = load_env_file(tmp.path(), false, &output);
        assert_eq!(vars["DOUBLE"], "hello world");
        assert_eq!(vars["SINGLE"], "foo bar");
        assert_eq!(vars["PLAIN"], "baz");
    }

    // detect_mode

    #[rstest]
    #[case::force_user_as_root(true, true, None, true)]
    #[case::force_user_non_root(true, false, None, true)]
    #[case::root_defaults_system(false, true, None, false)]
    #[case::non_root_defaults_user(false, false, None, true)]
    #[case::scope_system(false, false, Some("system"), false)]
    #[case::scope_user(false, true, Some("user"), true)]
    #[case::scope_empty_means_system(false, true, Some(""), false)]
    fn test_detect_mode(
        #[case] force_user: bool,
        #[case] is_root: bool,
        #[case] scope: Option<&str>,
        #[case] expected: bool,
    ) {
        assert_eq!(detect_mode(force_user, is_root, scope), expected);
    }

    // test_config

    #[test]
    fn test_config_creates_valid_config() {
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        assert!(cfg.is_user_mode);
        assert!(!cfg.verbose);
        assert!(!cfg.force);
    }

    // load_env_file

    #[test]
    fn load_env_file_verbose_logs_vars() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".env"), "PORT=8080").unwrap();

        let err_buf = crate::output::tests::TestWriter::new();
        let output = Output::new(Box::new(Vec::new()), Box::new(err_buf.clone()));
        let vars = load_env_file(tmp.path(), true, &output);
        assert_eq!(vars["PORT"], "8080");

        let stderr = err_buf.captured();
        assert!(stderr.contains("Loading environment from"));
        assert!(stderr.contains("Variables for substitution"));
        assert!(stderr.contains("PORT"));
    }

    // resolve_generator

    #[rstest]
    #[case::user_empty_override(true, Some(""), "user-generators")]
    #[case::user_no_override(true, None, "user-generators")]
    #[case::system_no_override(false, None, "system-generators")]
    #[case::custom_override(true, Some("/custom/generator"), "/custom/generator")]
    fn test_resolve_generator(
        #[case] user_mode: bool,
        #[case] override_path: Option<&str>,
        #[case] expected: &str,
    ) {
        let gen = resolve_generator(user_mode, override_path);
        assert!(gen.to_string_lossy().contains(expected));
    }

    // apply_flags

    #[test]
    fn apply_flags_verbose_logs_mode() {
        let err_buf = crate::output::tests::TestWriter::new();
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(err_buf.clone()));

        cfg.apply_flags(false, true, false);

        let stderr = err_buf.captured();
        assert!(stderr.contains("Running in"));
        assert!(stderr.contains("Source:"));
    }

    #[test]
    fn apply_flags_force_user_sets_user_mode() {
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.is_root = true;
        cfg.is_user_mode = false;

        cfg.apply_flags(true, false, false);

        assert!(cfg.is_user_mode);
    }

    #[test]
    fn apply_flags_force_flag() {
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));

        cfg.apply_flags(false, false, true);

        assert!(cfg.force);
    }

    #[test]
    fn apply_flags_with_quadcd_unit_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_dir = tmp.path().join("units");
        fs::create_dir_all(&unit_dir).unwrap();

        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.quadcd_unit_dirs = Some(unit_dir.to_string_lossy().to_string());

        cfg.apply_flags(false, false, false);

        assert_eq!(cfg.source_dir, unit_dir);
    }

    // resolve_source_dir

    #[rstest]
    #[case::empty_override_uses_default(Some(""), "/data")]
    #[case::custom_override(Some("/custom/path"), "/custom/path")]
    #[case::no_override_uses_default(None, "/data")]
    fn test_resolve_source_dir(#[case] override_path: Option<&str>, #[case] expected: &str) {
        assert_eq!(
            resolve_source_dir(override_path, Path::new("/data")),
            PathBuf::from(expected)
        );
    }

    // effective_source_dirs

    #[test]
    fn effective_source_dirs_with_quadcd_unit_dirs() {
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.quadcd_unit_dirs = Some("/some/dir".to_string());
        cfg.source_dir = PathBuf::from("/some/dir");
        cfg.env_vars.insert("KEY".to_string(), "val".to_string());

        let dirs = cfg.effective_source_dirs();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].0, PathBuf::from("/some/dir"));
        assert_eq!(dirs[0].1["KEY"], "val");
    }

    #[test]
    fn effective_source_dirs_missing_data_dir() {
        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.quadcd_unit_dirs = None;
        cfg.data_dir = PathBuf::from("/no/such/dir");

        let dirs = cfg.effective_source_dirs();
        assert!(dirs.is_empty());
    }

    #[test]
    fn effective_source_dirs_no_env_files_returns_empty_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(data_dir.join("subdir")).unwrap();

        let mut cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        cfg.quadcd_unit_dirs = None;
        cfg.data_dir = data_dir;

        let dirs = cfg.effective_source_dirs();
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].1.is_empty());
    }

    // merge_env_vars

    #[test]
    fn merge_env_vars_overrides_base() {
        let mut base = HashMap::new();
        base.insert("A".to_string(), "1".to_string());
        base.insert("B".to_string(), "2".to_string());

        let mut overrides = HashMap::new();
        overrides.insert("B".to_string(), "override".to_string());
        overrides.insert("C".to_string(), "3".to_string());

        let merged = merge_env_vars(&base, &overrides);
        assert_eq!(merged["A"], "1");
        assert_eq!(merged["B"], "override");
        assert_eq!(merged["C"], "3");
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn merge_env_vars_empty_overrides() {
        let mut base = HashMap::new();
        base.insert("A".to_string(), "1".to_string());
        let merged = merge_env_vars(&base, &HashMap::new());
        assert_eq!(merged, base);
    }

    #[test]
    fn merge_env_vars_empty_base() {
        let mut overrides = HashMap::new();
        overrides.insert("A".to_string(), "1".to_string());
        let merged = merge_env_vars(&HashMap::new(), &overrides);
        assert_eq!(merged, overrides);
    }

    // parse_git_timeout

    #[rstest]
    #[case::none_uses_default(None, 300)]
    #[case::valid_number(Some("60"), 60)]
    #[case::invalid_uses_default(Some("not-a-number"), 300)]
    #[case::zero(Some("0"), 0)]
    fn test_parse_git_timeout(#[case] input: Option<&str>, #[case] secs: u64) {
        assert_eq!(parse_git_timeout(input), Duration::from_secs(secs));
    }
}
