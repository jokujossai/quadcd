//! TOML-based configuration for continuous deployment.
//!
//! Defines the structure of the `quadcd.toml` config file and provides loading
//! from standard locations, with support for interval duration parsing.

use fundu::DurationParserBuilder;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// Top-level config file structure.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CDConfig {
    pub repositories: HashMap<String, RepoConfig>,
}

/// Configuration for a single git repository.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub url: String,
    pub branch: Option<String>,
    pub interval: Option<String>,
}

impl RepoConfig {
    /// Parse the `interval` field into a `Duration`.
    ///
    /// Supports time units like `s`, `m`, `h`, `d`, `w` and combined formats like `1h30m`.
    /// Returns `None` if no interval is configured or the format is invalid.
    pub fn interval_duration(&self) -> Option<Duration> {
        parse_interval(self.interval.as_deref()?)
    }
}

const DEFAULT_TICK: Duration = Duration::from_secs(10);

impl CDConfig {
    /// Return the smallest configured interval across all repositories.
    ///
    /// Used as the service loop tick so repos are checked as soon as they
    /// become due. Falls back to 10 seconds when no intervals are configured.
    pub fn min_interval(&self) -> Duration {
        self.repositories
            .values()
            .filter_map(|r| r.interval_duration())
            .min()
            .unwrap_or(DEFAULT_TICK)
    }

    /// Return the config file path by searching standard locations.
    ///
    /// Priority:
    /// 1. `quadcd_config` override (from `QUADCD_CONFIG` env var)
    /// 2. `~/.config/quadcd.toml` (user mode only)
    /// 3. `/etc/quadcd.toml` (system mode only)
    pub fn config_path(
        is_user_mode: bool,
        home: &str,
        quadcd_config: Option<&str>,
    ) -> Option<PathBuf> {
        // 1. QUADCD_CONFIG override
        if let Some(p) = quadcd_config.filter(|s| !s.is_empty()) {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path.canonicalize().unwrap_or(path));
            }
        }

        // 2. User config
        if is_user_mode {
            let path = PathBuf::from(format!("{home}/.config/quadcd.toml"));
            if path.exists() {
                return Some(path.canonicalize().unwrap_or(path));
            }
        }

        // 3. System config (only in system mode)
        if !is_user_mode {
            let path = PathBuf::from("/etc/quadcd.toml");
            if path.exists() {
                return Some(path.canonicalize().unwrap_or(path));
            }
        }

        None
    }

    /// Parse config from TOML string contents.
    pub fn parse(contents: &str) -> Result<CDConfig, String> {
        let cfg: CDConfig =
            toml::from_str(contents).map_err(|e| format!("failed to parse config: {e}"))?;
        for name in cfg.repositories.keys() {
            validate_repo_name(name)?;
        }
        Ok(cfg)
    }

    /// Load config from a specific file path.
    pub fn load_from_path(path: &std::path::Path) -> Result<CDConfig, String> {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        Self::parse(&contents)
    }
}

/// Validate that a repository name is safe for use as a filesystem path component.
///
/// Rejects empty names, `.`/`..`, and names containing characters outside
/// `[a-zA-Z0-9._-]` to prevent path traversal and shell injection.
fn validate_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("repository name must not be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("invalid repository name: '{name}'"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(format!(
            "repository name '{name}' contains invalid characters (allowed: a-z, A-Z, 0-9, '.', '_', '-')"
        ));
    }
    Ok(())
}

/// Parse a duration string into a `Duration` using the `fundu` crate.
///
/// Supports all default time units: `ns`, `Ms`, `ms`, `s`, `m`, `h`, `d`, `w`,
/// as well as combined formats like `1h30m`. Defaults to seconds when no unit is given.
pub(crate) fn parse_interval(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    const PARSER: fundu::DurationParser = DurationParserBuilder::new()
        .default_time_units()
        .parse_multiple(None)
        .build();

    PARSER.parse(s).ok().and_then(|d| d.try_into().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

    // parse_interval

    #[rstest]
    #[case::seconds("30s", 30)]
    #[case::minutes("5m", 300)]
    #[case::hours("1h", 3600)]
    #[case::days("2d", 172800)]
    #[case::weeks("1w", 604800)]
    #[case::compound("1h30m", 5400)]
    #[case::whitespace_trimmed("  30s  ", 30)]
    #[case::bare_number("60", 60)]
    fn parse_interval_valid(#[case] input: &str, #[case] secs: u64) {
        assert_eq!(parse_interval(input), Some(Duration::from_secs(secs)));
    }

    #[rstest]
    #[case::bad_suffix("10x")]
    #[case::suffix_only("s")]
    #[case::empty("")]
    #[case::negative("-5s")]
    fn parse_interval_invalid(#[case] input: &str) {
        assert_eq!(parse_interval(input), None);
    }

    // RepoConfig::interval_duration

    #[test]
    fn repo_config_interval_duration_some() {
        let rc = RepoConfig {
            url: "https://example.com".to_string(),
            branch: None,
            interval: Some("10m".to_string()),
        };
        assert_eq!(rc.interval_duration(), Some(Duration::from_secs(600)));
    }

    #[test]
    fn repo_config_interval_duration_none() {
        let rc = RepoConfig {
            url: "https://example.com".to_string(),
            branch: None,
            interval: None,
        };
        assert_eq!(rc.interval_duration(), None);
    }

    // CDConfig::parse

    #[test]
    fn parse_valid_toml() {
        let toml = r#"
[repositories.myapp]
url = "https://github.com/user/myapp.git"
branch = "main"
interval = "30s"

[repositories.other]
url = "https://github.com/user/other.git"
"#;
        let cfg = CDConfig::parse(toml).unwrap();
        assert_eq!(cfg.repositories.len(), 2);
        let myapp = &cfg.repositories["myapp"];
        assert_eq!(myapp.url, "https://github.com/user/myapp.git");
        assert_eq!(myapp.branch.as_deref(), Some("main"));
        assert_eq!(myapp.interval.as_deref(), Some("30s"));
        let other = &cfg.repositories["other"];
        assert_eq!(other.branch, None);
        assert_eq!(other.interval, None);
    }

    #[test]
    fn parse_empty_repos() {
        let toml = "[repositories]";
        let cfg = CDConfig::parse(toml).unwrap();
        assert!(cfg.repositories.is_empty());
    }

    #[test]
    fn parse_missing_url_field() {
        let toml = r#"
[repositories.bad]
branch = "main"
"#;
        assert!(CDConfig::parse(toml).is_err());
    }

    #[test]
    fn parse_invalid_toml() {
        assert!(CDConfig::parse("not valid toml [[[").is_err());
    }

    // CDConfig::config_path

    #[test]
    fn config_path_env_override() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_file = tmp.path().join("custom.toml");
        fs::write(&cfg_file, "").unwrap();
        let result = CDConfig::config_path(true, "/nonexistent", Some(cfg_file.to_str().unwrap()));
        assert_eq!(result, Some(cfg_file.canonicalize().unwrap()));
    }

    #[test]
    fn config_path_env_override_canonicalizes_relative_path() {
        // inotify always reports absolute paths; if QUADCD_CONFIG is a relative
        // path, the stored value must be canonicalized so the comparison in
        // is_config_event does not silently fail.
        static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = CWD_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let cfg_file = tmp.path().join("custom.toml");
        fs::write(&cfg_file, "").unwrap();

        let saved = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = CDConfig::config_path(true, "/nonexistent", Some("custom.toml"));
        std::env::set_current_dir(saved).unwrap();

        let canonical = cfg_file.canonicalize().unwrap();
        // Returned path must be the canonical absolute path
        assert_eq!(result, Some(canonical));
        // The raw relative string would not match inotify event paths
        assert_ne!(result, Some(PathBuf::from("custom.toml")));
    }

    #[test]
    fn config_path_env_override_missing_file() {
        let result = CDConfig::config_path(true, "/nonexistent", Some("/no/such/file.toml"));
        // Falls through because the file doesn't exist
        assert!(result.is_none() || result != Some(PathBuf::from("/no/such/file.toml")));
    }

    #[test]
    fn config_path_user_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        let cfg_file = config_dir.join("quadcd.toml");
        fs::write(&cfg_file, "").unwrap();
        let result = CDConfig::config_path(true, tmp.path().to_str().unwrap(), None);
        assert_eq!(result, Some(cfg_file.canonicalize().unwrap()));
    }

    #[test]
    fn config_path_no_files_found() {
        let result = CDConfig::config_path(false, "/nonexistent", None);
        // /etc/quadcd.toml likely doesn't exist in test environment
        if !PathBuf::from("/etc/quadcd.toml").exists() {
            assert!(result.is_none());
        }
    }

    #[test]
    fn config_path_user_mode_does_not_fall_back_to_system() {
        // In user mode without a user config, should NOT pick up /etc/quadcd.toml
        let result = CDConfig::config_path(true, "/nonexistent", None);
        assert!(result.is_none());
    }

    #[test]
    fn parse_unknown_field_error() {
        let toml = r#"
[repositories.myapp]
url = "https://github.com/user/myapp.git"
branche = "main"
"#;
        let err = CDConfig::parse(toml).unwrap_err();
        assert!(err.contains("unknown field"), "error was: {err}");
    }

    #[test]
    fn parse_error_includes_details() {
        let err = CDConfig::parse("not valid toml [[[").unwrap_err();
        assert!(err.contains("failed to parse config"), "error was: {err}");
    }

    #[test]
    fn load_from_path_missing_file() {
        let err = CDConfig::load_from_path(std::path::Path::new("/no/such/file.toml")).unwrap_err();
        assert!(err.contains("failed to read"), "error was: {err}");
    }

    #[test]
    fn load_from_path_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.toml");
        fs::write(&path, "not valid [[[").unwrap();
        let err = CDConfig::load_from_path(&path).unwrap_err();
        assert!(err.contains("failed to parse config"), "error was: {err}");
    }

    // CDConfig::min_interval

    #[test]
    fn min_interval_picks_smallest() {
        let toml = r#"
[repositories.fast]
url = "https://example.com/fast.git"
interval = "30s"

[repositories.slow]
url = "https://example.com/slow.git"
interval = "5m"
"#;
        let cfg = CDConfig::parse(toml).unwrap();
        assert_eq!(cfg.min_interval(), Duration::from_secs(30));
    }

    #[test]
    fn min_interval_no_intervals_returns_default() {
        let toml = r#"
[repositories.myapp]
url = "https://example.com/myapp.git"
"#;
        let cfg = CDConfig::parse(toml).unwrap();
        assert_eq!(cfg.min_interval(), Duration::from_secs(10));
    }

    #[test]
    fn min_interval_mixed() {
        let toml = r#"
[repositories.with_interval]
url = "https://example.com/a.git"
interval = "5s"

[repositories.without_interval]
url = "https://example.com/b.git"
"#;
        let cfg = CDConfig::parse(toml).unwrap();
        assert_eq!(cfg.min_interval(), Duration::from_secs(5));
    }

    #[test]
    fn min_interval_empty_repos() {
        let cfg = CDConfig::parse("[repositories]").unwrap();
        assert_eq!(cfg.min_interval(), Duration::from_secs(10));
    }

    // validate_repo_name

    #[rstest]
    #[case::simple("myapp")]
    #[case::with_dash("my-app")]
    #[case::with_underscore_dot("my_app.v2")]
    #[case::uppercase_digits("REPO123")]
    fn validate_repo_name_valid(#[case] name: &str) {
        assert!(validate_repo_name(name).is_ok());
    }

    #[rstest]
    #[case::empty("")]
    #[case::dot(".")]
    #[case::dotdot("..")]
    #[case::slash("foo/bar")]
    #[case::backslash("foo\\bar")]
    #[case::traversal("../../etc")]
    #[case::space("foo bar")]
    #[case::semicolon("a;b")]
    #[case::command_substitution("$(cmd)")]
    #[case::ampersand("a&b")]
    fn validate_repo_name_invalid(#[case] name: &str) {
        assert!(validate_repo_name(name).is_err());
    }

    #[test]
    fn parse_rejects_traversal_name() {
        let toml = r#"
[repositories."../../etc/cron.d"]
url = "https://example.com/evil.git"
"#;
        let err = CDConfig::parse(toml).unwrap_err();
        assert!(err.contains("invalid characters"), "error was: {err}");
    }

    #[test]
    fn parse_accepts_valid_names() {
        let toml = r#"
[repositories.myapp]
url = "https://example.com/myapp.git"

[repositories.my-app_v2]
url = "https://example.com/other.git"
"#;
        assert!(CDConfig::parse(toml).is_ok());
    }
}
