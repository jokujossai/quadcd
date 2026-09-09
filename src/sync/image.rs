//! Image pulling trait and extraction logic for container image pre-pulling.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::{collections::HashMap, fs};

use crate::config::Config;

/// A container image reference with optional authentication settings.
#[derive(Debug)]
pub struct ImageRef {
    pub image: String,
    pub auth_file: Option<String>,
    pub tls_verify: Option<bool>,
    /// Extra arguments forwarded verbatim to `podman pull`.
    pub podman_args: Vec<String>,
    /// Passed as `podman pull --policy`, so podman decides whether to pull.
    pub pull_policy: PullPolicy,
}

/// Pull policy from `Pull=` (`.container`) or `Policy=` (`.image`).
/// Ordered from least to most eager to pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PullPolicy {
    Never,
    /// `.container` default.
    Missing,
    Newer,
    /// `.image` default (`podman pull`'s own).
    Always,
}

impl PullPolicy {
    /// `None` for anything but the four values podman accepts.
    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "always" => Some(Self::Always),
            "missing" => Some(Self::Missing),
            "never" => Some(Self::Never),
            "newer" => Some(Self::Newer),
            _ => None,
        }
    }

    /// The value to pass to `podman pull --policy=`.
    pub fn as_podman_arg(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Missing => "missing",
            Self::Never => "never",
            Self::Newer => "newer",
        }
    }
}

const PULL_PASSTHROUGH_FLAGS: &[&str] = &[
    "--authfile",
    "--tls-verify",
    "--creds",
    "--cert-dir",
    "--os",
    "--arch",
    "--variant",
    "--platform",
    "--decryption-key",
];

/// Tokenize a `PodmanArgs=` value into individual argument strings.
///
/// Splits on unquoted whitespace; single- and double-quoted spans are kept
/// together (quotes are stripped). No backslash escaping is supported.
fn split_args(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in s.chars() {
        match in_quote {
            Some(q) if ch == q => in_quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => in_quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Keep only args from `PodmanArgs=` in a `.container` file that are valid
/// for `podman pull`.  Handles both `--flag=value` and `--flag value` forms.
fn filter_pull_args(args: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut iter = args.into_iter().peekable();
    while let Some(token) = iter.next() {
        if let Some(eq_pos) = token.find('=') {
            let flag = &token[..eq_pos];
            if PULL_PASSTHROUGH_FLAGS.contains(&flag) {
                result.push(token);
            }
        } else if PULL_PASSTHROUGH_FLAGS.contains(&token.as_str()) {
            result.push(token);
            if let Some(val) = iter.next() {
                result.push(val);
            }
        }
        // else: runtime-only flag — skip
    }
    result
}

/// Abstraction over container image pulling.
///
/// `Podman` shells out to podman; tests can substitute a mock that records
/// calls without requiring a running container runtime.
pub trait ImagePuller {
    fn pull(&self, image: &ImageRef, cfg: &Config);
}

/// Extract container image references from changed `.container` and `.image`
/// files.
///
/// Reads each matching file from `source_dir`, applies variable substitution
/// with the provided `env_vars`, and returns a list of image references.
/// Image values ending in `.image` or `.build` are skipped (they are
/// references to quadlet units, not actual image URLs).
///
/// NOTE! Does not handle whitespaces, comments, quoted values, or multi-line values.
pub(crate) fn extract_images(
    changed_files: &[String],
    source_dir: &Path,
    env_vars: &HashMap<String, String>,
    verbose: bool,
    output: &crate::output::Output,
) -> Vec<ImageRef> {
    let mut images: Vec<ImageRef> = Vec::new();

    for filename in changed_files {
        let is_container = filename.ends_with(".container");
        let is_image = filename.ends_with(".image");
        if !is_container && !is_image {
            continue;
        }

        let file_path = source_dir.join(filename);
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                if verbose {
                    let _ = writeln!(
                        output.err(),
                        "[quadcd] Warning: could not read {}: {e}",
                        file_path.display()
                    );
                }
                continue;
            }
        };
        let content = crate::install::envsubst(&content, env_vars);

        let primary_section = if is_container { "Container" } else { "Image" };
        let (policy_key, mut pull_policy) = if is_container {
            ("Pull=", PullPolicy::Missing)
        } else {
            ("Policy=", PullPolicy::Always)
        };
        let mut current_section: Option<&str> = None;

        let mut image_val = None;
        let mut auth_file = None;
        let mut tls_verify = None;
        let mut podman_args_raw: Vec<String> = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                current_section = Some(&trimmed[1..trimmed.len() - 1]);
                continue;
            }
            if current_section == Some(primary_section) {
                if let Some(val) = trimmed.strip_prefix("Image=") {
                    let val = val.trim();
                    if !val.is_empty() {
                        image_val = Some(val.to_string());
                    }
                }
                if let Some(val) = trimmed.strip_prefix(policy_key) {
                    if let Some(policy) = PullPolicy::parse(val) {
                        pull_policy = policy;
                    }
                }
                if let Some(val) = trimmed.strip_prefix("PodmanArgs=") {
                    podman_args_raw.extend(split_args(val));
                }
            }
            if is_image && current_section == Some("Image") {
                if let Some(val) = trimmed.strip_prefix("AuthFile=") {
                    let val = val.trim();
                    if !val.is_empty() {
                        auth_file = Some(val.to_string());
                    }
                }
                if let Some(val) = trimmed.strip_prefix("TLSVerify=") {
                    let val = val.trim();
                    tls_verify = match val {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                }
            }
        }

        if pull_policy == PullPolicy::Never {
            continue;
        }
        if let Some(image) = image_val {
            // Skip references to .image and .build quadlet units
            if image.ends_with(".image") || image.ends_with(".build") {
                continue;
            }
            let podman_args = if is_image {
                podman_args_raw
            } else {
                filter_pull_args(podman_args_raw)
            };
            images.push(ImageRef {
                image,
                auth_file,
                tls_verify,
                podman_args,
                pull_policy,
            });
        }
    }

    images
}

/// Deduplicate image references by image name, keeping the first occurrence
/// with the most eager pull policy of all its duplicates.
///
/// NOTE! Does not handle auth_file or tls_verify.
pub(crate) fn dedup_images(images: &mut Vec<ImageRef>) {
    let mut policies: HashMap<String, PullPolicy> = HashMap::new();
    for r in images.iter() {
        let policy = policies.entry(r.image.clone()).or_insert(r.pull_policy);
        *policy = (*policy).max(r.pull_policy);
    }
    let mut seen = HashSet::new();
    images.retain_mut(|r| {
        r.pull_policy = policies[&r.image];
        seen.insert(r.image.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::collections::HashMap;
    use std::fs;

    fn extract_images_helper(
        changed_files: &[String],
        source_dir: &Path,
        env_vars: &HashMap<String, String>,
    ) -> Vec<ImageRef> {
        let output = crate::output::Output::new(Box::new(Vec::new()), Box::new(Vec::new()));
        extract_images(changed_files, source_dir, env_vars, false, &output)
    }

    // extract_images

    #[rstest]
    #[case::container_image(
        "app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\n",
        Some("quay.io/podman/hello:latest"),
        None,
        None
    )]
    #[case::image_file_with_auth(
        "app.image",
        "[Image]\nImage=registry.example.com/app:latest\nAuthFile=/run/secrets/auth.json\nTLSVerify=false\n",
        Some("registry.example.com/app:latest"),
        Some("/run/secrets/auth.json"),
        Some(false),
    )]
    #[case::skips_image_unit_ref(
        "app.container",
        "[Container]\nImage=myapp.image\n",
        None,
        None,
        None
    )]
    #[case::skips_build_unit_ref(
        "app.container",
        "[Container]\nImage=myapp.build\n",
        None,
        None,
        None
    )]
    #[case::skips_service_file("app.service", "Image=shouldnt-match\n", None, None, None)]
    #[case::skips_volume_file("data.volume", "", None, None, None)]
    #[case::no_auth_from_container(
        "app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\nAuthFile=/some/path\n",
        Some("quay.io/podman/hello:latest"),
        None,
        None
    )]
    #[case::skips_pull_never(
        "app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\nPull=never\n",
        None,
        None,
        None
    )]
    #[case::pull_always_not_skipped(
        "app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\nPull=always\n",
        Some("quay.io/podman/hello:latest"),
        None,
        None
    )]
    #[case::ignores_image_in_service_section(
        "app.container",
        "[Service]\nEnvironment=Image=wrong\n[Container]\nImage=correct:tag\n",
        Some("correct:tag"),
        None,
        None
    )]
    #[case::ignores_pull_never_in_wrong_section(
        "app.container",
        "[Service]\nPull=never\n[Container]\nImage=quay.io/podman/hello:latest\n",
        Some("quay.io/podman/hello:latest"),
        None,
        None
    )]
    #[case::pull_never_in_container_suppresses(
        "app.container",
        "[Container]\nImage=quay.io/podman/hello:latest\nPull=never\n",
        None,
        None,
        None
    )]
    #[case::auth_from_wrong_section_ignored(
        "app.image",
        "[Unit]\nAuthFile=/wrong\n[Image]\nImage=reg.io/app:1\nAuthFile=/correct\n",
        Some("reg.io/app:1"),
        Some("/correct"),
        None
    )]
    #[case::tls_verify_from_wrong_section_ignored(
        "app.image",
        "[Unit]\nTLSVerify=false\n[Image]\nImage=reg.io/app:1\nTLSVerify=true\n",
        Some("reg.io/app:1"),
        None,
        Some(true)
    )]
    #[case::no_section_header_ignored(
        "app.container",
        "Image=nosection\n[Container]\nImage=real:1\n",
        Some("real:1"),
        None,
        None
    )]
    #[case::image_only_in_image_section(
        "app.image",
        "[Container]\nImage=wrong\n[Image]\nImage=correct:latest\n",
        Some("correct:latest"),
        None,
        None
    )]
    fn extract_images_single_file(
        #[case] filename: &str,
        #[case] content: &str,
        #[case] expected_image: Option<&str>,
        #[case] expected_auth: Option<&str>,
        #[case] expected_tls: Option<bool>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(filename), content).unwrap();

        let images = extract_images_helper(&[filename.to_string()], tmp.path(), &HashMap::new());

        match expected_image {
            Some(img) => {
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].image, img);
                assert_eq!(images[0].auth_file.as_deref(), expected_auth);
                assert_eq!(images[0].tls_verify, expected_tls);
            }
            None => assert!(images.is_empty()),
        }
    }

    #[test]
    fn extract_images_applies_envsubst() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.container"),
            "[Container]\nImage=${REGISTRY}/app:${TAG}\n",
        )
        .unwrap();

        let mut vars = HashMap::new();
        vars.insert("REGISTRY".to_string(), "ghcr.io/myorg".to_string());
        vars.insert("TAG".to_string(), "v2".to_string());

        let images = extract_images_helper(&["app.container".to_string()], tmp.path(), &vars);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].image, "ghcr.io/myorg/app:v2");
    }

    // pull policy

    #[rstest]
    #[case::container_default_missing(
        "app.container",
        "[Container]\nImage=a\n",
        PullPolicy::Missing
    )]
    #[case::container_missing(
        "app.container",
        "[Container]\nImage=a\nPull=missing\n",
        PullPolicy::Missing
    )]
    #[case::container_always(
        "app.container",
        "[Container]\nImage=a\nPull=always\n",
        PullPolicy::Always
    )]
    #[case::container_newer(
        "app.container",
        "[Container]\nImage=a\nPull=newer\n",
        PullPolicy::Newer
    )]
    #[case::container_unknown_value(
        "app.container",
        "[Container]\nImage=a\nPull=bogus\n",
        PullPolicy::Missing
    )]
    #[case::container_ignores_policy_key(
        "app.container",
        "[Container]\nImage=a\nPolicy=always\n",
        PullPolicy::Missing
    )]
    #[case::image_default_always("app.image", "[Image]\nImage=a\n", PullPolicy::Always)]
    #[case::image_missing("app.image", "[Image]\nImage=a\nPolicy=missing\n", PullPolicy::Missing)]
    #[case::image_ignores_pull_key(
        "app.image",
        "[Image]\nImage=a\nPull=missing\n",
        PullPolicy::Always
    )]
    fn extract_images_pull_policy(
        #[case] filename: &str,
        #[case] content: &str,
        #[case] expected: PullPolicy,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(filename), content).unwrap();

        let images = extract_images_helper(&[filename.to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].pull_policy, expected);
    }

    #[test]
    fn extract_images_skips_image_with_policy_never() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.image"),
            "[Image]\nImage=a\nPolicy=never\n",
        )
        .unwrap();

        let images = extract_images_helper(&["app.image".to_string()], tmp.path(), &HashMap::new());
        assert!(images.is_empty());
    }

    #[test]
    fn dedup_images_keeps_most_eager_policy() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.container"), "[Container]\nImage=x\n").unwrap();
        fs::write(tmp.path().join("b.image"), "[Image]\nImage=x\n").unwrap();
        fs::write(
            tmp.path().join("c.container"),
            "[Container]\nImage=x\nPull=newer\n",
        )
        .unwrap();

        let mut images = extract_images_helper(
            &[
                "a.container".to_string(),
                "b.image".to_string(),
                "c.container".to_string(),
            ],
            tmp.path(),
            &HashMap::new(),
        );
        dedup_images(&mut images);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].pull_policy, PullPolicy::Always);
    }

    #[test]
    fn dedup_images_removes_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("a.container"),
            "[Container]\nImage=quay.io/podman/hello:latest\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("b.container"),
            "[Container]\nImage=quay.io/podman/hello:latest\n",
        )
        .unwrap();

        let mut images = extract_images_helper(
            &["a.container".to_string(), "b.container".to_string()],
            tmp.path(),
            &HashMap::new(),
        );
        dedup_images(&mut images);
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn extract_images_skips_missing_files() {
        let tmp = tempfile::tempdir().unwrap();

        let images = extract_images_helper(
            &["missing.container".to_string()],
            tmp.path(),
            &HashMap::new(),
        );
        assert!(images.is_empty());
    }

    #[test]
    fn extract_images_container_with_image_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("web.container"),
            "[Container]\nImage=web.image\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("web.image"),
            "[Image]\nImage=quay.io/podman/hello:latest\nAuthFile=/run/auth.json\n",
        )
        .unwrap();

        let images = extract_images_helper(
            &["web.container".to_string(), "web.image".to_string()],
            tmp.path(),
            &HashMap::new(),
        );
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].image, "quay.io/podman/hello:latest");
        assert_eq!(images[0].auth_file.as_deref(), Some("/run/auth.json"));
    }

    // split_args

    #[test]
    fn split_args_simple() {
        assert_eq!(
            split_args("--authfile=/run/auth.json --os=linux"),
            vec!["--authfile=/run/auth.json", "--os=linux"]
        );
    }

    #[test]
    fn split_args_double_quoted() {
        assert_eq!(
            split_args("--creds=\"user:pass word\""),
            vec!["--creds=user:pass word"]
        );
    }

    #[test]
    fn split_args_single_quoted() {
        assert_eq!(
            split_args("--creds='user:pass word'"),
            vec!["--creds=user:pass word"]
        );
    }

    #[test]
    fn split_args_extra_whitespace() {
        assert_eq!(split_args("  --os=linux  "), vec!["--os=linux"]);
    }

    #[test]
    fn split_args_empty() {
        assert!(split_args("").is_empty());
        assert!(split_args("   ").is_empty());
    }

    // filter_pull_args

    #[test]
    fn filter_pull_args_keeps_allowlisted_eq_form() {
        let result = filter_pull_args(vec![
            "--authfile=/run/auth.json".to_string(),
            "--network=host".to_string(),
            "--os=linux".to_string(),
        ]);
        assert_eq!(result, vec!["--authfile=/run/auth.json", "--os=linux"]);
    }

    #[test]
    fn filter_pull_args_keeps_allowlisted_space_form() {
        let result = filter_pull_args(vec![
            "--authfile".to_string(),
            "/run/auth.json".to_string(),
            "--network".to_string(),
            "host".to_string(),
        ]);
        assert_eq!(result, vec!["--authfile", "/run/auth.json"]);
    }

    #[test]
    fn filter_pull_args_drops_all_runtime_flags() {
        let result = filter_pull_args(vec![
            "--network=host".to_string(),
            "--cap-add=SYS_ADMIN".to_string(),
            "--volume=/data:/data".to_string(),
        ]);
        assert!(result.is_empty());
    }

    // PodmanArgs= in extract_images

    #[test]
    fn container_podman_args_pull_flag_included() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.container"),
            "[Container]\nImage=quay.io/app:1\nPodmanArgs=--authfile=/run/auth.json\n",
        )
        .unwrap();

        let images =
            extract_images_helper(&["app.container".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].podman_args, vec!["--authfile=/run/auth.json"]);
    }

    #[test]
    fn container_podman_args_runtime_flag_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.container"),
            "[Container]\nImage=quay.io/app:1\nPodmanArgs=--network=host\n",
        )
        .unwrap();

        let images =
            extract_images_helper(&["app.container".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert!(images[0].podman_args.is_empty());
    }

    #[test]
    fn container_podman_args_mixed_keeps_only_pull_compatible() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.container"),
            "[Container]\nImage=quay.io/app:1\nPodmanArgs=--authfile=/f --network=host --os=linux\n",
        )
        .unwrap();

        let images =
            extract_images_helper(&["app.container".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].podman_args, vec!["--authfile=/f", "--os=linux"]);
    }

    #[test]
    fn image_podman_args_all_passed_through() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.image"),
            "[Image]\nImage=quay.io/app:1\nPodmanArgs=--policy=always\n",
        )
        .unwrap();

        let images = extract_images_helper(&["app.image".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].podman_args, vec!["--policy=always"]);
    }

    #[test]
    fn image_podman_args_multiple_lines_accumulated() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.image"),
            "[Image]\nImage=quay.io/app:1\nPodmanArgs=--os=linux\nPodmanArgs=--arch=amd64\n",
        )
        .unwrap();

        let images = extract_images_helper(&["app.image".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].podman_args, vec!["--os=linux", "--arch=amd64"]);
    }

    #[test]
    fn container_podman_args_ignored_outside_container_section() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("app.container"),
            "[Unit]\nPodmanArgs=--authfile=/wrong\n[Container]\nImage=quay.io/app:1\n",
        )
        .unwrap();

        let images =
            extract_images_helper(&["app.container".to_string()], tmp.path(), &HashMap::new());
        assert_eq!(images.len(), 1);
        assert!(images[0].podman_args.is_empty());
    }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::new_without_default)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;

    pub struct MockImagePuller {
        pub pulled: RefCell<Vec<String>>,
    }

    impl MockImagePuller {
        pub fn new() -> Self {
            Self {
                pulled: RefCell::new(Vec::new()),
            }
        }
    }

    impl ImagePuller for MockImagePuller {
        fn pull(&self, image: &ImageRef, _cfg: &Config) {
            self.pulled.borrow_mut().push(image.image.clone());
        }
    }
}
