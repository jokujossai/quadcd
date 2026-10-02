//! `Watch=`: mark a unit changed when other files in its repo change.
//!
//! A unit's own file is not always the whole of its definition: a `.build`
//! unit is only as current as its `Containerfile` and build context. `Watch=`
//! globs in the `[X-QuadCD]` section name those files, relative to the unit
//! file's directory, and a sync whose diff touches one treats the unit as
//! changed even though its own file is not.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::config::Config;
use crate::install::{find_files, QUADLET_EXTENSIONS, SYSTEMD_EXTENSIONS};

use super::settings::{parse_settings, SECTION};
use super::vcs::UnitChanges;

/// Add to `changes.changed` every unit file in `repo_dir` whose `Watch=`
/// globs match one of `changes.other`.
///
/// Runs per repository, while the diff's paths can still be tied to the repo
/// they came from. Unit files are read from the new tree, so a deleted file
/// matches only a unit that still exists. Paths are repo-relative, as
/// `git diff` reports them.
pub(crate) fn mark_watched(repo_dir: &Path, changes: &mut UnitChanges, cfg: &Config) {
    if changes.other.is_empty() {
        return;
    }

    let files = find_files(repo_dir, QUADLET_EXTENSIONS)
        .into_iter()
        .chain(find_files(repo_dir, SYSTEMD_EXTENSIONS));
    for path in files {
        let Some(rel) = repo_relative(repo_dir, &path) else {
            continue;
        };
        if changes.changed.contains(&rel) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        // Other warnings about the section are reported when sync reads it
        // for activation; only the ones specific to resolving globs are here.
        let (settings, _) = parse_settings(&content);
        let unit_dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);

        for glob in &settings.watch {
            let pattern = match resolve(unit_dir, glob) {
                Ok(p) => p,
                Err(e) => {
                    let _ = writeln!(
                        cfg.output.err(),
                        "[quadcd] Warning: {rel}: [{SECTION}] ignoring Watch={glob}: {e}"
                    );
                    continue;
                }
            };
            if let Some(hit) = changes.other.iter().find(|p| glob_match(&pattern, p)) {
                let _ = writeln!(
                    cfg.output.err(),
                    "[quadcd] {rel}: changed via Watch={glob} ({hit})"
                );
                changes.changed.push(rel);
                break;
            }
        }
    }
}

/// `path` relative to `repo_dir`, with `/` separators.
fn repo_relative(repo_dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(repo_dir).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// Join a `Watch=` glob onto the unit file's directory and normalise `.` and
/// `..`, giving a repo-relative pattern.
fn resolve(unit_dir: &str, glob: &str) -> Result<String, String> {
    if glob.starts_with('/') {
        return Err("absolute paths are not supported".to_string());
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in unit_dir.split('/').chain(glob.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err("path escapes the repository".to_string());
                }
            }
            s => segments.push(s),
        }
    }
    Ok(segments.join("/"))
}

/// Match a repo-relative path against a glob.
///
/// `*` matches any run of characters within one path segment, `?` a single
/// character, and a `**` segment any number of whole segments, including none.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| match_segments(rest, &path[i..])),
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(seg, tail)| match_segment(first, seg) && match_segments(rest, tail)),
    }
}

/// Wildcard match of one segment, backtracking to the most recent `*`.
fn match_segment(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;
    use crate::output::tests::TestWriter;
    use rstest::rstest;

    #[rstest]
    #[case::exact("Containerfile", "Containerfile", true)]
    #[case::exact_other("Containerfile", "Containerfile.podman", false)]
    #[case::star_suffix("Containerfile*", "Containerfile.podman", true)]
    #[case::star_stays_in_segment("ctx/*", "ctx/sub/file", false)]
    #[case::star_in_middle("ctx/*.sh", "ctx/entry.sh", true)]
    #[case::question("v?.txt", "v1.txt", true)]
    #[case::question_needs_char("v?.txt", "v.txt", false)]
    #[case::double_star_deep("rootless/**", "rootless/a/b/c", true)]
    #[case::double_star_direct("rootless/**", "rootless/a", true)]
    #[case::double_star_other_dir("rootless/**", "rootlessx/a", false)]
    #[case::double_star_middle("a/**/z.conf", "a/z.conf", true)]
    #[case::double_star_middle_deep("a/**/z.conf", "a/b/c/z.conf", true)]
    #[case::double_star_everything("**", "any/path", true)]
    #[case::backtracking("*a*b", "xaxxab", true)]
    fn glob_match_cases(#[case] pattern: &str, #[case] path: &str, #[case] expected: bool) {
        assert_eq!(glob_match(pattern, path), expected, "{pattern} vs {path}");
    }

    #[rstest]
    #[case::top_level("", "Containerfile", Ok("Containerfile"))]
    #[case::nested("builds/act", "rootless/**", Ok("builds/act/rootless/**"))]
    #[case::dot("builds", "./Containerfile", Ok("builds/Containerfile"))]
    #[case::parent("builds/act", "../shared/*", Ok("builds/shared/*"))]
    #[case::escapes("builds", "../../etc", Err("path escapes the repository"))]
    #[case::absolute("", "/etc/passwd", Err("absolute paths are not supported"))]
    fn resolve_cases(#[case] dir: &str, #[case] glob: &str, #[case] expected: Result<&str, &str>) {
        let expected = expected.map(String::from).map_err(String::from);
        assert_eq!(resolve(dir, glob), expected);
    }

    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = tmp.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        tmp
    }

    fn changes_with_other(other: &[&str]) -> UnitChanges {
        UnitChanges {
            other: other.iter().map(|s| s.to_string()).collect(),
            ..UnitChanges::default()
        }
    }

    #[test]
    fn mark_watched_relative_to_unit_directory() {
        let repo = repo_with(&[
            (
                "act/act.build",
                "[Build]\nImageTag=localhost/act\n[X-QuadCD]\nWatch=Containerfile\nWatch=rootless/**\n",
            ),
            (
                "tor/tor.build",
                "[Build]\nImageTag=localhost/tor\n[X-QuadCD]\nWatch=Containerfile\n",
            ),
        ]);
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        let mut changes = changes_with_other(&["act/rootless/entry.sh", "README.md"]);

        mark_watched(repo.path(), &mut changes, &cfg);

        assert_eq!(changes.changed, vec!["act/act.build".to_string()]);
    }

    #[test]
    fn mark_watched_counts_deleted_files_and_logs_the_match() {
        // `other` holds deleted paths too; the unit itself still exists.
        let repo = repo_with(&[(
            "app.build",
            "[Build]\nImageTag=localhost/app\n[X-QuadCD]\nWatch=ctx/**\n",
        )]);
        let err = TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err.clone()));
        let mut changes = changes_with_other(&["ctx/removed.conf"]);

        mark_watched(repo.path(), &mut changes, &cfg);

        assert_eq!(changes.changed, vec!["app.build".to_string()]);
        assert!(
            err.captured()
                .contains("app.build: changed via Watch=ctx/** (ctx/removed.conf)"),
            "got: {}",
            err.captured()
        );
    }

    #[test]
    fn mark_watched_does_not_duplicate_an_already_changed_unit() {
        let repo = repo_with(&[(
            "app.build",
            "[Build]\nImageTag=localhost/app\n[X-QuadCD]\nWatch=Containerfile\n",
        )]);
        let cfg = test_config(Box::new(Vec::new()), Box::new(Vec::new()));
        let mut changes = changes_with_other(&["Containerfile"]);
        changes.changed.push("app.build".to_string());

        mark_watched(repo.path(), &mut changes, &cfg);

        assert_eq!(changes.changed, vec!["app.build".to_string()]);
    }

    #[test]
    fn mark_watched_ignores_and_warns_about_escaping_glob() {
        let repo = repo_with(&[(
            "app.build",
            "[Build]\nImageTag=localhost/app\n[X-QuadCD]\nWatch=../other-repo/**\n",
        )]);
        let err = TestWriter::new();
        let cfg = test_config(Box::new(Vec::new()), Box::new(err.clone()));
        let mut changes = changes_with_other(&["other-repo/x"]);

        mark_watched(repo.path(), &mut changes, &cfg);

        assert!(changes.changed.is_empty());
        assert!(
            err.captured()
                .contains("ignoring Watch=../other-repo/**: path escapes the repository"),
            "got: {}",
            err.captured()
        );
    }
}
