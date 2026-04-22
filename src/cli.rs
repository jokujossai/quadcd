use std::io::Write;
use std::path::Path;

use crate::output::Output;

pub(crate) const TOP_LEVEL_USAGE: &str = "Usage:\n  quadcd generate [-v] [-no-kmsg-log] [-user] [-dryrun] normal-dir [early-dir] [late-dir]\n  quadcd sync [--service] [--sync-only] [--force] [--accept-new-host-keys] [-i|--interactive] [--user] [-v]\n  quadcd version\n  quadcd help";
pub(crate) const SYNC_USAGE: &str =
    "Usage: quadcd sync [--service] [--sync-only] [--force] [--accept-new-host-keys] [-i|--interactive] [--user] [-v]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedCommand {
    Help,
    Version,
    Generate(GenerateInvocation),
    Sync(SyncInvocation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerateInvocation {
    pub(crate) program: String,
    pub(crate) original_args: Vec<String>,
    pub(crate) verbose: bool,
    pub(crate) dryrun: bool,
    pub(crate) force_user: bool,
    pub(crate) positional: Vec<String>,
    pub(crate) show_help: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncInvocation {
    pub(crate) verbose: bool,
    pub(crate) force: bool,
    pub(crate) force_user: bool,
    pub(crate) service: bool,
    pub(crate) sync_only: bool,
    pub(crate) accept_new_host_keys: bool,
    pub(crate) interactive: bool,
    pub(crate) show_help: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    message: Option<String>,
    usage: ParseUsage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseUsage {
    TopLevel,
    Generate { program: String },
    Sync,
}

impl ParseError {
    fn unknown_subcommand(other: &str) -> Self {
        Self {
            message: Some(format!("Error: unknown subcommand: {other}")),
            usage: ParseUsage::TopLevel,
        }
    }

    fn missing_command() -> Self {
        Self {
            message: None,
            usage: ParseUsage::TopLevel,
        }
    }

    fn generate_invalid(arg: &str, program: &str) -> Self {
        Self {
            message: Some(format!("Error: invalid argument: {arg}")),
            usage: ParseUsage::Generate {
                program: program.to_string(),
            },
        }
    }

    fn generate_missing(program: &str) -> Self {
        Self {
            message: Some("Error: missing required argument: normal-dir".to_string()),
            usage: ParseUsage::Generate {
                program: program.to_string(),
            },
        }
    }

    fn sync_invalid(arg: &str) -> Self {
        Self {
            message: Some(format!("Error: invalid argument for sync: {arg}")),
            usage: ParseUsage::Sync,
        }
    }

    pub(crate) fn emit(&self, output: &Output) {
        if let Some(message) = &self.message {
            let _ = writeln!(output.err(), "{message}");
        }

        match &self.usage {
            ParseUsage::TopLevel => {
                let _ = writeln!(output.err(), "{TOP_LEVEL_USAGE}");
            }
            ParseUsage::Generate { program } => {
                let _ = writeln!(
                    output.err(),
                    "Usage: {program} generate [-v] [-no-kmsg-log] [-user] [-dryrun] [-version] normal-dir [early-dir] [late-dir]"
                );
            }
            ParseUsage::Sync => {
                let _ = writeln!(output.err(), "{SYNC_USAGE}");
            }
        }
    }
}

pub(crate) fn parse_cli(
    args: &[String],
    systemd_scope: Option<&str>,
) -> Result<ParsedCommand, ParseError> {
    if args.iter().any(|a| a == "-version") {
        return Ok(ParsedCommand::Version);
    }

    if is_systemd_generator_invocation(args, systemd_scope) {
        return parse_generate(&args[0], &args[1..]);
    }

    match args.get(1).map(|s| s.as_str()) {
        Some("generate") => parse_generate(&args[0], &args[2..]),
        Some("sync") => parse_sync(&args[2..]),
        Some("version") => Ok(ParsedCommand::Version),
        Some("help") | Some("-h") | Some("-help") | Some("--help") => Ok(ParsedCommand::Help),
        Some(other) => Err(ParseError::unknown_subcommand(other)),
        None => Err(ParseError::missing_command()),
    }
}

fn parse_generate(program: &str, args: &[String]) -> Result<ParsedCommand, ParseError> {
    let mut verbose = false;
    let mut dryrun = false;
    let mut force_user = false;
    let mut positional: Vec<String> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-v" => verbose = true,
            "-no-kmsg-log" => {}
            "-user" => force_user = true,
            "-dryrun" => {
                dryrun = true;
                verbose = true;
            }
            "-version" => return Ok(ParsedCommand::Version),
            "-h" | "-help" | "--help" => {
                return Ok(ParsedCommand::Generate(GenerateInvocation {
                    program: program.to_string(),
                    original_args: args.to_vec(),
                    verbose,
                    dryrun,
                    force_user,
                    positional,
                    show_help: true,
                }))
            }
            _ if !arg.starts_with('-') => positional.push(arg.clone()),
            _ => return Err(ParseError::generate_invalid(arg, program)),
        }
    }

    if !dryrun && positional.is_empty() {
        return Err(ParseError::generate_missing(program));
    }

    Ok(ParsedCommand::Generate(GenerateInvocation {
        program: program.to_string(),
        original_args: args.to_vec(),
        verbose,
        dryrun,
        force_user,
        positional,
        show_help: false,
    }))
}

fn parse_sync(args: &[String]) -> Result<ParsedCommand, ParseError> {
    let mut verbose = false;
    let mut force = false;
    let mut force_user = false;
    let mut service = false;
    let mut sync_only = false;
    let mut accept_new_host_keys = false;
    let mut interactive = false;

    for arg in args {
        match arg.as_str() {
            "-v" => verbose = true,
            "--force" => force = true,
            "--user" => force_user = true,
            "--service" => service = true,
            "--sync-only" => sync_only = true,
            "--accept-new-host-keys" => accept_new_host_keys = true,
            "-i" | "--interactive" => interactive = true,
            "-h" | "-help" | "--help" => {
                return Ok(ParsedCommand::Sync(SyncInvocation {
                    verbose,
                    force,
                    force_user,
                    service,
                    sync_only,
                    accept_new_host_keys,
                    interactive,
                    show_help: true,
                }))
            }
            _ => return Err(ParseError::sync_invalid(arg)),
        }
    }

    Ok(ParsedCommand::Sync(SyncInvocation {
        verbose,
        force,
        force_user,
        service,
        sync_only,
        accept_new_host_keys,
        interactive,
        show_help: false,
    }))
}

pub(crate) fn is_systemd_generator_invocation(
    args: &[String],
    systemd_scope: Option<&str>,
) -> bool {
    let positional_count = args.len().saturating_sub(1);
    if positional_count != 1 && positional_count != 3 {
        return false;
    }

    if systemd_scope.is_none() {
        return false;
    }

    if let Some(first_arg) = args.get(1) {
        Path::new(first_arg).is_dir()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autodetect_rejects_wrong_arg_count() {
        let args: Vec<String> = ["quadcd", "/dir1", "/dir2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(!is_systemd_generator_invocation(&args, Some("system")));
    }

    #[test]
    fn autodetect_rejects_missing_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let args = vec![
            "quadcd".to_string(),
            tmp.path().to_string_lossy().to_string(),
        ];
        assert!(!is_systemd_generator_invocation(&args, None));
    }

    #[test]
    fn autodetect_rejects_non_directory_arg() {
        let args = vec!["quadcd".to_string(), "--some-flag".to_string()];
        assert!(!is_systemd_generator_invocation(&args, Some("system")));
    }

    #[test]
    fn autodetect_accepts_valid_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        let args = vec![
            "quadcd".to_string(),
            tmp.path().to_string_lossy().to_string(),
        ];
        assert!(is_systemd_generator_invocation(&args, Some("system")));
    }

    #[test]
    fn autodetect_accepts_three_directory_args() {
        let tmp = tempfile::tempdir().unwrap();
        let d1 = tmp.path().join("a");
        let d2 = tmp.path().join("b");
        let d3 = tmp.path().join("c");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        std::fs::create_dir_all(&d3).unwrap();
        let args = vec![
            "quadcd".to_string(),
            d1.to_string_lossy().to_string(),
            d2.to_string_lossy().to_string(),
            d3.to_string_lossy().to_string(),
        ];
        assert!(is_systemd_generator_invocation(&args, Some("system")));
    }

    #[test]
    fn parse_cli_routes_version_anywhere() {
        let args = vec![
            "quadcd".to_string(),
            "sync".to_string(),
            "-version".to_string(),
        ];
        assert_eq!(parse_cli(&args, None).unwrap(), ParsedCommand::Version);
    }

    #[test]
    fn parse_cli_detects_systemd_generator_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        let args = vec![
            "quadcd".to_string(),
            tmp.path().to_string_lossy().to_string(),
        ];
        match parse_cli(&args, Some("system")).unwrap() {
            ParsedCommand::Generate(invocation) => {
                assert_eq!(
                    invocation.original_args,
                    vec![tmp.path().to_string_lossy().to_string()]
                );
                assert_eq!(
                    invocation.positional,
                    vec![tmp.path().to_string_lossy().to_string()]
                );
            }
            other => panic!("expected generate invocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_cli_returns_help_for_top_level_help_flags() {
        for flag in ["-h", "-help", "--help"] {
            let args = vec!["quadcd".to_string(), flag.to_string()];
            assert_eq!(parse_cli(&args, None).unwrap(), ParsedCommand::Help);
        }
    }

    #[test]
    fn parse_cli_rejects_unknown_subcommand() {
        let args = vec!["quadcd".to_string(), "bogus".to_string()];
        let err = parse_cli(&args, None).unwrap_err();
        assert_eq!(
            err,
            ParseError {
                message: Some("Error: unknown subcommand: bogus".to_string()),
                usage: ParseUsage::TopLevel,
            }
        );
    }

    #[test]
    fn parse_cli_rejects_missing_command() {
        let args = vec!["quadcd".to_string()];
        let err = parse_cli(&args, None).unwrap_err();
        assert_eq!(
            err,
            ParseError {
                message: None,
                usage: ParseUsage::TopLevel,
            }
        );
    }

    #[test]
    fn parse_generate_sets_dryrun_and_verbose() {
        let args = vec!["-dryrun".to_string(), "/tmp/out".to_string()];
        match parse_generate("quadcd", &args).unwrap() {
            ParsedCommand::Generate(invocation) => {
                assert!(invocation.dryrun);
                assert!(invocation.verbose);
                assert_eq!(invocation.positional, vec!["/tmp/out".to_string()]);
            }
            other => panic!("expected generate invocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_generate_returns_help_invocation() {
        let args = vec!["--help".to_string()];
        match parse_generate("quadcd", &args).unwrap() {
            ParsedCommand::Generate(invocation) => assert!(invocation.show_help),
            other => panic!("expected generate invocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_generate_rejects_invalid_flag() {
        let args = vec!["--bogus".to_string()];
        let err = parse_generate("quadcd", &args).unwrap_err();
        assert_eq!(
            err,
            ParseError {
                message: Some("Error: invalid argument: --bogus".to_string()),
                usage: ParseUsage::Generate {
                    program: "quadcd".to_string(),
                },
            }
        );
    }

    #[test]
    fn parse_generate_rejects_missing_normal_dir_without_dryrun() {
        let args = Vec::new();
        let err = parse_generate("quadcd", &args).unwrap_err();
        assert_eq!(
            err,
            ParseError {
                message: Some("Error: missing required argument: normal-dir".to_string()),
                usage: ParseUsage::Generate {
                    program: "quadcd".to_string(),
                },
            }
        );
    }

    #[test]
    fn parse_sync_parses_flags() {
        let args = vec![
            "-v".to_string(),
            "--force".to_string(),
            "--user".to_string(),
            "--service".to_string(),
            "--sync-only".to_string(),
            "--accept-new-host-keys".to_string(),
            "--interactive".to_string(),
        ];
        match parse_sync(&args).unwrap() {
            ParsedCommand::Sync(invocation) => {
                assert!(invocation.verbose);
                assert!(invocation.force);
                assert!(invocation.force_user);
                assert!(invocation.service);
                assert!(invocation.sync_only);
                assert!(invocation.accept_new_host_keys);
                assert!(invocation.interactive);
                assert!(!invocation.show_help);
            }
            other => panic!("expected sync invocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_sync_returns_help_invocation() {
        let args = vec!["--help".to_string()];
        match parse_sync(&args).unwrap() {
            ParsedCommand::Sync(invocation) => assert!(invocation.show_help),
            other => panic!("expected sync invocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_sync_rejects_invalid_flag() {
        let args = vec!["--bogus".to_string()];
        let err = parse_sync(&args).unwrap_err();
        assert_eq!(
            err,
            ParseError {
                message: Some("Error: invalid argument for sync: --bogus".to_string()),
                usage: ParseUsage::Sync,
            }
        );
    }
}
