//! Plain-text and JSON renderers for the status report.

use std::io::{self, Write};
use std::time::Duration;

use crate::config::Config;

use super::{Mode, RepoState, RepoStatus, ServiceStatus, StatusReport};

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::User => "user",
        Mode::System => "system",
    }
}

pub(crate) fn write_json(report: &StatusReport, cfg: &Config) -> io::Result<()> {
    let mut out = cfg.output.out();
    let json = serde_json::to_string_pretty(report).map_err(io::Error::other)?;
    writeln!(out, "{json}")?;
    Ok(())
}

pub(crate) fn write_plain(report: &StatusReport, cfg: &Config) -> io::Result<()> {
    let mut out = cfg.output.out();
    let config_label = report
        .config_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<none>".to_string());
    writeln!(
        out,
        "Repositories (mode: {}, config: {})",
        mode_label(report.mode),
        config_label
    )?;
    if report.repos.is_empty() {
        writeln!(out, "  (none configured)")?;
    } else {
        writeln!(
            out,
            "  {:<20} {:<16} {:<32} HEAD",
            "NAME", "BRANCH", "STATE"
        )?;
        for r in &report.repos {
            writeln!(
                out,
                "  {:<20} {:<16} {:<32} {}",
                truncate(&r.name, 20),
                truncate(&r.branch, 16),
                truncate(&render_repo_state(r), 32),
                head_short(r),
            )?;
        }
    }

    writeln!(out)?;
    writeln!(out, "Services")?;
    if report.services.is_empty() {
        writeln!(out, "  (no managed units found)")?;
    } else {
        writeln!(
            out,
            "  {:<28} {:<10} {:<10} {:<10} {:<7} {:<8} {:>4} {:<10} NOTE",
            "UNIT", "ACTIVE", "SUB", "ENABLED", "RELOAD", "RESTART", "N", "UPTIME"
        )?;
        for s in &report.services {
            writeln!(
                out,
                "  {:<28} {:<10} {:<10} {:<10} {:<7} {:<8} {:>4} {:<10} {}",
                truncate(&s.unit, 28),
                truncate(&s.active_state, 10),
                truncate(&s.sub_state, 10),
                truncate(&s.enabled, 10),
                if s.needs_daemon_reload {
                    "needed"
                } else {
                    "ok"
                },
                if s.restart_pending { "pending" } else { "ok" },
                s.n_restarts,
                format_uptime(s.uptime),
                service_note(s),
            )?;
        }
    }

    if !report.warnings.is_empty() {
        writeln!(out)?;
        writeln!(out, "Warnings")?;
        for w in &report.warnings {
            writeln!(out, "  - {w}")?;
        }
    }
    Ok(())
}

fn render_repo_state(r: &RepoStatus) -> String {
    match &r.state {
        RepoState::Missing => "missing".to_string(),
        RepoState::UpToDate => "up-to-date".to_string(),
        RepoState::Ahead { commits } => format!("ahead {commits}"),
        RepoState::Behind { commits } => format!("behind {commits}"),
        RepoState::Diverged { ahead, behind } => format!("diverged +{ahead}/-{behind}"),
        RepoState::UrlMismatch { actual, .. } => format!("url mismatch ({actual})"),
        RepoState::Error { message } => format!("error: {message}"),
    }
}

fn head_short(r: &RepoStatus) -> String {
    match &r.head_sha {
        Some(sha) => sha.chars().take(7).collect(),
        None => "—".to_string(),
    }
}

fn service_note(s: &ServiceStatus) -> String {
    let mut notes = Vec::new();
    if s.needs_daemon_reload {
        notes.push("daemon-reload pending");
    }
    if s.restart_pending {
        notes.push("restart pending");
    }
    if s.restart_loop_suspected {
        notes.push("restart loop?");
    }
    if s.active_state != "active" && s.active_state != "activating" {
        notes.push("not active");
    }
    notes.join("; ")
}

fn format_uptime(uptime: Option<Duration>) -> String {
    let Some(d) = uptime else {
        return "—".to_string();
    };
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{}h{}m", hours, mins % 60);
    }
    let days = hours / 24;
    format!("{}d{}h", days, hours % 24)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_report() -> StatusReport {
        StatusReport {
            mode: Mode::User,
            config_path: Some(PathBuf::from("/etc/quadcd.toml")),
            repos: vec![RepoStatus {
                name: "app".to_string(),
                url: "https://example.com/app.git".to_string(),
                branch: "main".to_string(),
                local_path: PathBuf::from("/tmp/app"),
                state: RepoState::UpToDate,
                fetched: true,
                head_sha: Some("abc1234567".to_string()),
            }],
            services: vec![ServiceStatus {
                unit: "web.service".to_string(),
                origin_repo: "app".to_string(),
                source_file: PathBuf::from("/tmp/app/web.container"),
                active_state: "active".to_string(),
                sub_state: "running".to_string(),
                result: "success".to_string(),
                enabled: "enabled".to_string(),
                needs_daemon_reload: false,
                restart_pending: false,
                n_restarts: 0,
                uptime: Some(Duration::from_secs(3600 + 120)),
                restart_loop_suspected: false,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn plain_text_contains_columns_and_values() {
        let report = sample_report();
        let stdout_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(stdout_buf.clone()), Box::new(Vec::new()));
        write_plain(&report, &cfg).unwrap();
        let out = stdout_buf.captured();
        assert!(out.contains("Repositories"));
        assert!(out.contains("up-to-date"));
        assert!(out.contains("abc1234"));
        assert!(out.contains("web.service"));
        assert!(out.contains("enabled"));
    }

    #[test]
    fn json_is_valid_and_contains_fields() {
        let report = sample_report();
        let stdout_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(stdout_buf.clone()), Box::new(Vec::new()));
        write_json(&report, &cfg).unwrap();
        let out = stdout_buf.captured();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mode"], "user");
        assert_eq!(v["repos"][0]["state"]["state"], "up-to-date");
        assert_eq!(v["services"][0]["unit"], "web.service");
        assert_eq!(v["services"][0]["uptime"], 3720);
    }

    #[test]
    fn warnings_section_rendered_when_present() {
        let mut report = sample_report();
        report.warnings = vec!["something fishy".to_string()];
        let stdout_buf = crate::output::tests::TestWriter::new();
        let cfg = crate::config::test_config(Box::new(stdout_buf.clone()), Box::new(Vec::new()));
        write_plain(&report, &cfg).unwrap();
        let out = stdout_buf.captured();
        assert!(out.contains("Warnings"), "out: {out}");
        assert!(out.contains("something fishy"), "out: {out}");
    }

    #[test]
    fn format_uptime_handles_ranges() {
        assert_eq!(format_uptime(None), "—");
        assert_eq!(format_uptime(Some(Duration::from_secs(45))), "45s");
        assert_eq!(format_uptime(Some(Duration::from_secs(300))), "5m");
        assert_eq!(format_uptime(Some(Duration::from_secs(3661))), "1h1m");
        assert_eq!(format_uptime(Some(Duration::from_secs(3 * 86400))), "3d0h");
    }
}
