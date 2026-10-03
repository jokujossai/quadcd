//! Unit file content rewriting: `${VAR}` substitution and `SourcePath=`.

use std::collections::HashMap;
use std::path::Path;

/// Perform `${VAR}` substitution in `content` using the provided key-value map.
///
/// Only the exact `${KEY}` syntax is replaced; `$KEY` without braces is not
/// matched. Substitution is single-pass: variables appearing in substituted
/// values are not expanded.
pub fn envsubst(content: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find(['}', '$', '{']);
        if end.is_some_and(|e| rest.as_bytes()[e] == b'}') {
            let end = end.unwrap();
            let key = &rest[..end];
            if let Some(value) = vars.get(key) {
                result.push_str(value);
            } else {
                result.push_str("${");
                result.push_str(key);
                result.push('}');
            }
            rest = &rest[end + 1..];
        } else {
            // No valid closing brace — emit the literal "${" and
            // continue scanning from where we are (so inner "${" can match).
            result.push_str("${");
        }
    }
    result.push_str(rest);
    result
}

/// Inject or replace `SourcePath=` in a unit file's `[Unit]` section.
///
/// If the content already has a `SourcePath=` line it is replaced.
/// If a `[Unit]` section exists, the directive is inserted right after it.
/// Otherwise a `[Unit]` section is prepended.
pub(super) fn set_source_path(content: &str, source_path: &Path) -> String {
    let source_line = format!("SourcePath={}", source_path.display());
    let mut result = String::with_capacity(content.len() + source_line.len() + 20);
    let mut injected = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("SourcePath=") {
            continue;
        }

        result.push_str(line);
        result.push('\n');

        if !injected && trimmed == "[Unit]" {
            result.push_str(&source_line);
            result.push('\n');
            injected = true;
        }
    }

    if !injected {
        let mut prefixed = format!("[Unit]\n{source_line}\n\n");
        prefixed.push_str(&result);
        return prefixed;
    }

    result
}

/// Remove duplicate `SourcePath=` lines, keeping only the first occurrence.
///
/// The podman generator adds its own `SourcePath=` pointing at the temporary
/// quadlet directory. Because quadcd already injects the real `SourcePath=`
/// first, dropping duplicates preserves the correct value.
pub fn clean_duplicate_source_path(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut seen = false;

    for line in content.lines() {
        if line.trim().starts_with("SourcePath=") {
            if seen {
                continue;
            }
            seen = true;
        }
        result.push_str(line);
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // envsubst

    #[test]
    fn envsubst_replaces_variables() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "world".to_string());
        vars.insert("PORT".to_string(), "8080".to_string());
        let result = envsubst("Hello ${NAME} on port ${PORT}", &vars);
        assert_eq!(result, "Hello world on port 8080");
    }

    #[test]
    fn envsubst_missing_var_left_as_is() {
        let vars = HashMap::new();
        let result = envsubst("${MISSING} stays", &vars);
        assert_eq!(result, "${MISSING} stays");
    }

    #[test]
    fn envsubst_empty_vars_map() {
        let vars = HashMap::new();
        let result = envsubst("no vars here", &vars);
        assert_eq!(result, "no vars here");
    }

    #[test]
    fn envsubst_bare_dollar_not_replaced() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = envsubst("$FOO and ${FOO}", &vars);
        assert_eq!(result, "$FOO and bar");
    }

    #[test]
    fn envsubst_multiple_occurrences() {
        let mut vars = HashMap::new();
        vars.insert("X".to_string(), "y".to_string());
        let result = envsubst("${X}${X}${X}", &vars);
        assert_eq!(result, "yyy");
    }

    #[test]
    fn envsubst_empty_value() {
        let mut vars = HashMap::new();
        vars.insert("EMPTY".to_string(), String::new());
        let result = envsubst("before${EMPTY}after", &vars);
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn envsubst_cascading_var_in_value_not_expanded() {
        let mut vars = HashMap::new();
        vars.insert("IMAGE_TAG".to_string(), "latest".to_string());
        vars.insert(
            "IMAGE".to_string(),
            "quay.io/podman/hello:${IMAGE_TAG}".to_string(),
        );
        let result = envsubst("Image=${IMAGE}", &vars);
        assert_eq!(result, "Image=quay.io/podman/hello:${IMAGE_TAG}");
    }

    #[test]
    fn envsubst_deterministic_regardless_of_insertion_order() {
        let mut vars1 = HashMap::new();
        vars1.insert("A".to_string(), "${B}".to_string());
        vars1.insert("B".to_string(), "hello".to_string());

        // Run many times to exercise different HashMap orderings
        let results: std::collections::HashSet<String> =
            (0..50).map(|_| envsubst("${A} ${B}", &vars1)).collect();
        assert_eq!(results.len(), 1, "envsubst must be deterministic");
        assert_eq!(results.into_iter().next().unwrap(), "${B} hello");
    }

    #[test]
    fn envsubst_malformed_no_closing_brace() {
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        let result = envsubst("${FOO} and ${NOCLOSE", &vars);
        assert_eq!(result, "bar and ${NOCLOSE");
    }

    #[test]
    fn envsubst_nested_dollar_brace_skips_outer() {
        let mut vars = HashMap::new();
        vars.insert("test".to_string(), "value".to_string());
        assert_eq!(envsubst("${${test}", &vars), "${value");
    }

    #[test]
    fn envsubst_adjacent_vars() {
        let mut vars = HashMap::new();
        vars.insert("A".to_string(), "1".to_string());
        vars.insert("B".to_string(), "2".to_string());
        assert_eq!(envsubst("${A}${B}", &vars), "12");
    }

    #[test]
    fn envsubst_preserves_utf8() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "wörld".to_string());
        let result = envsubst("héllo ${NAME} café", &vars);
        assert_eq!(result, "héllo wörld café");
    }

    // set_source_path

    #[test]
    fn set_source_path_injects_after_unit_section() {
        let content = "[Unit]\nDescription=test\n\n[Service]\nExecStart=/bin/true\n";
        let result = set_source_path(content, Path::new("/src/app.container"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/app.container\nDescription=test\n\n[Service]\nExecStart=/bin/true\n"
        );
    }

    #[test]
    fn set_source_path_prepends_unit_section_when_missing() {
        let content = "[Service]\nExecStart=/bin/true\n";
        let result = set_source_path(content, Path::new("/src/app.service"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/app.service\n\n[Service]\nExecStart=/bin/true\n"
        );
    }

    #[test]
    fn set_source_path_replaces_existing() {
        let content = "[Unit]\nSourcePath=/tmp/old\nDescription=test\n";
        let result = set_source_path(content, Path::new("/src/real.container"));
        assert_eq!(
            result,
            "[Unit]\nSourcePath=/src/real.container\nDescription=test\n"
        );
    }

    #[test]
    fn set_source_path_no_content() {
        let result = set_source_path("", Path::new("/src/app.container"));
        assert_eq!(result, "[Unit]\nSourcePath=/src/app.container\n\n");
    }

    // clean_duplicate_source_path

    #[test]
    fn clean_duplicate_source_path_keeps_first() {
        let content = "[Unit]\nSourcePath=/real/path\nSourcePath=/tmp/fake\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nSourcePath=/real/path\nDescription=test\n");
    }

    #[test]
    fn clean_duplicate_source_path_single_is_kept() {
        let content = "[Unit]\nSourcePath=/only/one\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nSourcePath=/only/one\nDescription=test\n");
    }

    #[test]
    fn clean_duplicate_source_path_none_is_noop() {
        let content = "[Unit]\nDescription=test\n";
        let result = clean_duplicate_source_path(content);
        assert_eq!(result, "[Unit]\nDescription=test\n");
    }
}
