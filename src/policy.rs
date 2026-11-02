//! The policy file, `.orchestral/policy.md`: settings the binary reads, in
//! YAML frontmatter, and rules Claude reads, as the prose after it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::git::git;
use crate::outcome::Failure;

pub const PATH: &str = ".orchestral/policy.md";

/// The branch the policy is read from, so a worker can't change its own
/// limits from a branch.
const MAIN: &str = "origin/main";

#[derive(Debug)]
pub struct Policy {
    pub settings: Settings,
    /// The prose after the frontmatter.
    pub rules: String,
}

#[derive(Debug, PartialEq)]
pub struct Settings {
    pub workers: u32,
    pub plan_limit_percent: u8,
    pub fleet_token_budget: u64,
    pub default_model: String,
    pub default_effort: String,
    pub merge_method: MergeMethod,
    pub supervisor_merges: bool,
    pub design_variants: u32,
    pub nag_minutes: Vec<u32>,
    pub preview: Option<Preview>,
    pub review_command: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

#[derive(Debug, PartialEq)]
pub struct Preview {
    pub setup: Option<String>,
    pub start: PreviewStart,
    pub max_running: u32,
}

/// How a PR's preview is reached.
#[derive(Debug, PartialEq)]
pub enum PreviewStart {
    /// A command the supervisor runs.
    Run(String),
    /// A URL the host gives each PR, with `{pr}` and `{branch}` filled in.
    Url(String),
}

/// The frontmatter as written, before defaults and checks.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    workers: Option<u32>,
    plan_limit_percent: Option<u8>,
    fleet_token_budget: Option<u64>,
    default_model: Option<String>,
    default_effort: Option<String>,
    merge_method: Option<MergeMethod>,
    supervisor_merges: Option<bool>,
    design_variants: Option<u32>,
    nag_minutes: Option<Vec<u32>>,
    preview: Option<PreviewFrontmatter>,
    review_command: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewFrontmatter {
    setup: Option<String>,
    run: Option<String>,
    url: Option<String>,
    max_running: Option<u32>,
}

impl Policy {
    /// Reads the policy for the repository containing `cwd`, from `origin/main`,
    /// or from the main checkout until `origin/main` exists.
    pub fn load(cwd: &Path) -> Result<Policy, Failure> {
        let source = Source::find(cwd)?;
        let text = source.read(PATH)?.ok_or_else(|| {
            Failure::Error(format!(
                "there's no {PATH} in {}; add one",
                source.describe()
            ))
        })?;
        let has_contributing = source.read("CONTRIBUTING.md")?.is_some();
        Policy::parse(&text, has_contributing)
    }

    /// Parses a policy file. `has_contributing` sets the default for
    /// `supervisor_merges`.
    pub fn parse(text: &str, has_contributing: bool) -> Result<Policy, Failure> {
        let invalid = |message: String| Failure::Error(format!("{PATH} is invalid: {message}"));
        // Editors may add a byte order mark or Windows line endings.
        let text = text
            .strip_prefix('\u{feff}')
            .unwrap_or(text)
            .replace("\r\n", "\n");
        let (yaml, rules) = split_frontmatter(&text).ok_or_else(|| {
            invalid("it has to start with frontmatter between `---` lines".into())
        })?;
        let front: Frontmatter =
            yaml_serde::from_str(yaml).map_err(|err| invalid(err.to_string()))?;

        let fleet_token_budget = match front.fleet_token_budget {
            Some(budget) if budget > 0 => budget,
            Some(_) => return Err(invalid("`fleet_token_budget` has to be above 0".into())),
            None => return Err(invalid("`fleet_token_budget` is required".into())),
        };
        let preview = front
            .preview
            .map(|preview| {
                let start = match (preview.run, preview.url) {
                    (Some(run), None) => PreviewStart::Run(run),
                    (None, Some(url)) => PreviewStart::Url(url),
                    (Some(_), Some(_)) => {
                        return Err(invalid("`preview` takes `run` or `url`, not both".into()));
                    }
                    (None, None) => return Err(invalid("`preview` needs `run` or `url`".into())),
                };
                Ok(Preview {
                    setup: preview.setup,
                    start,
                    max_running: preview.max_running.unwrap_or(3),
                })
            })
            .transpose()?;

        let settings = Settings {
            workers: front.workers.unwrap_or(3),
            plan_limit_percent: front.plan_limit_percent.unwrap_or(85),
            fleet_token_budget,
            default_model: front
                .default_model
                .unwrap_or_else(|| "claude-opus-5-5".into()),
            default_effort: front.default_effort.unwrap_or_else(|| "high".into()),
            merge_method: front.merge_method.unwrap_or(MergeMethod::Squash),
            supervisor_merges: front.supervisor_merges.unwrap_or(!has_contributing),
            design_variants: front.design_variants.unwrap_or(4),
            nag_minutes: front
                .nag_minutes
                .unwrap_or_else(|| vec![15, 30, 60, 120, 240]),
            preview,
            review_command: front.review_command,
        };
        Ok(Policy {
            settings,
            rules: rules.trim().to_owned(),
        })
    }
}

/// Splits `---\n<yaml>\n---\n<rules>` into its two parts.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let mut end = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..end], &rest[end + line.len()..]));
        }
        end += line.len();
    }
    None
}

/// Where the policy is read from.
enum Source {
    /// A commit, read through `git show`.
    Branch { repo: PathBuf },
    /// The main checkout's files, before `origin/main` exists.
    Checkout { dir: PathBuf },
}

impl Source {
    fn find(cwd: &Path) -> Result<Source, Failure> {
        let common = git(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let repo = cwd.to_owned();
        let has_main = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{MAIN}^{{commit}}"))
            .current_dir(&repo)
            .output()
            .map_err(|err| Failure::Error(format!("can't run git: {err}")))?
            .status
            .success();
        if has_main {
            return Ok(Source::Branch { repo });
        }
        let dir = Path::new(common.trim_end())
            .parent()
            .expect("a git common directory has a parent")
            .to_owned();
        Ok(Source::Checkout { dir })
    }

    /// The file's contents, or `None` when it doesn't exist there.
    fn read(&self, path: &str) -> Result<Option<String>, Failure> {
        match self {
            Source::Branch { repo } => {
                // An empty listing means the file isn't there. Any other
                // failure is an error, not a missing file.
                if git(repo, &["ls-tree", "--name-only", MAIN, "--", path])?.is_empty() {
                    return Ok(None);
                }
                git(repo, &["show", &format!("{MAIN}:{path}")]).map(Some)
            }
            Source::Checkout { dir } => match std::fs::read_to_string(dir.join(path)) {
                Ok(text) => Ok(Some(text)),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(Failure::Error(format!(
                    "{}: {err}",
                    dir.join(path).display()
                ))),
            },
        }
    }

    fn describe(&self) -> String {
        match self {
            Source::Branch { .. } => MAIN.to_owned(),
            Source::Checkout { dir } => dir.display().to_string(),
        }
    }
}

/// Runs git in `dir` and returns its stdout. A non-zero exit is an error
/// carrying git's message.
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(frontmatter: &str) -> Result<Settings, String> {
        let text = format!("---\n{frontmatter}---\nRules go here.\n");
        Policy::parse(&text, false)
            .map(|policy| policy.settings)
            .map_err(|failure| match failure {
                Failure::Error(message) => message,
                Failure::Refused { reason, .. } => reason,
            })
    }

    #[test]
    fn a_budget_alone_gives_every_default() {
        let settings = parse("fleet_token_budget: 3000000\n").unwrap();

        assert_eq!(
            settings,
            Settings {
                workers: 3,
                plan_limit_percent: 85,
                fleet_token_budget: 3_000_000,
                default_model: "claude-opus-5-5".into(),
                default_effort: "high".into(),
                merge_method: MergeMethod::Squash,
                supervisor_merges: true,
                design_variants: 4,
                nag_minutes: vec![15, 30, 60, 120, 240],
                preview: None,
                review_command: None,
            }
        );
    }

    #[test]
    fn supervisor_merges_defaults_off_with_a_contributing_file() {
        let text = "---\nfleet_token_budget: 1\n---\n";

        assert!(
            !Policy::parse(text, true)
                .unwrap()
                .settings
                .supervisor_merges
        );
    }

    #[test]
    fn an_explicit_supervisor_merges_wins() {
        let on = "---\nfleet_token_budget: 1\nsupervisor_merges: true\n---\n";
        let off = "---\nfleet_token_budget: 1\nsupervisor_merges: false\n---\n";

        assert!(Policy::parse(on, true).unwrap().settings.supervisor_merges);
        assert!(
            !Policy::parse(off, false)
                .unwrap()
                .settings
                .supervisor_merges
        );
    }

    #[test]
    fn the_rules_are_the_prose_after_the_frontmatter() {
        let text = "---\nfleet_token_budget: 1\n---\n\nPropose a design first.\nAlways ask me about auth.\n";

        let policy = Policy::parse(text, false).unwrap();

        assert_eq!(
            policy.rules,
            "Propose a design first.\nAlways ask me about auth."
        );
    }

    #[test]
    fn a_full_policy_parses() {
        let settings = parse(
            "workers: 2
plan_limit_percent: 70
fleet_token_budget: 3000000 # required
default_model: claude-sonnet-5-5
default_effort: medium
merge_method: rebase
supervisor_merges: false
design_variants: 2
nag_minutes: [10, 20]
preview:
  setup: pnpm install
  run: pnpm dev
  max_running: 1
review_command: /refine
",
        )
        .unwrap();

        assert_eq!(settings.workers, 2);
        assert_eq!(settings.merge_method, MergeMethod::Rebase);
        assert_eq!(settings.nag_minutes, [10, 20]);
        assert_eq!(
            settings.preview,
            Some(Preview {
                setup: Some("pnpm install".into()),
                start: PreviewStart::Run("pnpm dev".into()),
                max_running: 1,
            })
        );
        assert_eq!(settings.review_command.as_deref(), Some("/refine"));
    }

    #[test]
    fn a_preview_url_replaces_run() {
        let settings =
            parse("fleet_token_budget: 1\npreview:\n  url: https://pr-{pr}.example.com/{branch}\n")
                .unwrap();

        assert_eq!(
            settings.preview.unwrap().start,
            PreviewStart::Url("https://pr-{pr}.example.com/{branch}".into())
        );
    }

    #[test]
    fn a_preview_defaults_to_three_running() {
        let settings = parse("fleet_token_budget: 1\npreview:\n  run: pnpm dev\n").unwrap();

        assert_eq!(settings.preview.unwrap().max_running, 3);
    }

    #[test]
    fn accepts_a_byte_order_mark_and_windows_line_endings() {
        let text = "\u{feff}---\r\nfleet_token_budget: 1\r\n---\r\nRules.\r\n";

        assert_eq!(Policy::parse(text, false).unwrap().rules, "Rules.");
    }

    #[test]
    fn rejects_an_unknown_key() {
        let error = parse("fleet_token_budget: 1\nworker: 3\n").unwrap_err();

        assert!(error.contains("unknown field `worker`"), "{error}");
    }

    #[test]
    fn rejects_an_unknown_preview_key() {
        let error = parse("fleet_token_budget: 1\npreview:\n  command: pnpm dev\n").unwrap_err();

        assert!(error.contains("unknown field `command`"), "{error}");
    }

    #[test]
    fn rejects_a_missing_budget() {
        let error = parse("workers: 3\n").unwrap_err();

        assert!(
            error.contains("`fleet_token_budget` is required"),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_invalid_budget() {
        for budget in ["0", "-5", "lots", "1.5"] {
            let result = parse(&format!("fleet_token_budget: {budget}\n"));

            assert!(result.is_err(), "{budget} was accepted");
        }
    }

    #[test]
    fn rejects_a_preview_with_run_and_url() {
        let error = parse("fleet_token_budget: 1\npreview:\n  run: pnpm dev\n  url: https://x\n")
            .unwrap_err();

        assert!(error.contains("not both"), "{error}");
    }

    #[test]
    fn rejects_a_preview_without_run_or_url() {
        let error = parse("fleet_token_budget: 1\npreview:\n  setup: pnpm install\n").unwrap_err();

        assert!(error.contains("needs `run` or `url`"), "{error}");
    }

    #[test]
    fn rejects_an_unknown_merge_method() {
        assert!(parse("fleet_token_budget: 1\nmerge_method: octopus\n").is_err());
    }

    #[test]
    fn rejects_a_file_without_frontmatter() {
        let error = Policy::parse("Just rules.\n", false).unwrap_err();

        assert!(matches!(error, Failure::Error(message) if message.contains("frontmatter")));
    }
}
