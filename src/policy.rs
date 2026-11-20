//! The policy: settings the binary reads, in YAML frontmatter, and rules
//! Claude reads, as the prose after it. It lives in the operator's config
//! directory, not the repository (ADR-0013): a global `policy.md`, layered
//! under a per-repository `<owner>/<repo>/policy.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::git::git;
use crate::outcome::Failure;

/// Where a repository kept its policy before it moved to the config
/// directory. Only read to move it, and to say it needs moving.
pub const TRACKED_PATH: &str = ".orchestral/policy.md";

const FILE: &str = "policy.md";

/// The branch repository files are read from, so a worker can't change them
/// from a branch.
const MAIN: &str = "origin/main";

#[derive(Debug, Serialize)]
pub struct Policy {
    pub settings: Settings,
    /// The prose after the frontmatter, global rules first.
    pub rules: String,
    /// The files it was read from, global first.
    pub sources: Vec<PathBuf>,
}

#[derive(Debug, PartialEq, Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_command: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Preview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    #[serde(flatten)]
    pub start: PreviewStart,
    pub max_running: u32,
}

/// How a PR's preview is reached.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
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

/// One policy file, checked on its own.
struct Layer {
    front: Frontmatter,
    preview: Option<Preview>,
    rules: String,
}

/// Where a repository's policy files go.
#[derive(Debug, Serialize)]
pub struct Files {
    /// `policy.md`, for every repository.
    pub global: PathBuf,
    /// `<owner>/<repo>/policy.md`, for the GitHub repository `origin` points
    /// at. `None` when `origin` isn't on GitHub.
    pub repo: Option<PathBuf>,
}

impl Files {
    /// The policy files for the repository containing `cwd`, in `config`.
    pub fn for_repo(config: &Path, cwd: &Path) -> Result<Files, Failure> {
        // The URL as configured, before any `insteadOf` rewrite.
        let output = Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
            .current_dir(cwd)
            .output()
            .map_err(|err| Failure::Error(format!("can't run git: {err}")))?;
        let url = String::from_utf8_lossy(&output.stdout);
        Ok(Files {
            global: config.join(FILE),
            repo: github_repo(url.trim()).map(|repo| config.join(repo).join(FILE)),
        })
    }

    /// The per-repository file when there is one, or else the global one:
    /// where `init` puts a new or moved policy.
    pub fn target(&self) -> &Path {
        self.repo.as_deref().unwrap_or(&self.global)
    }

    /// The files that exist, global first.
    pub fn existing(&self) -> Vec<PathBuf> {
        std::iter::once(&self.global)
            .chain(&self.repo)
            .filter(|path| path.is_file())
            .cloned()
            .collect()
    }
}

/// Orchestral's config directory: `$XDG_CONFIG_HOME/orchestral`, or
/// `~/.config/orchestral` when that's unset or, as the XDG spec says to
/// treat it, relative.
pub fn config_dir() -> Result<PathBuf, Failure> {
    if let Some(dir) =
        std::env::var_os("XDG_CONFIG_HOME").filter(|dir| Path::new(dir).is_absolute())
    {
        return Ok(PathBuf::from(dir).join("orchestral"));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| Failure::Error("neither XDG_CONFIG_HOME nor HOME is set".into()))?;
    Ok(PathBuf::from(home).join(".config/orchestral"))
}

/// `owner/repo`, lowercased, from a GitHub remote URL in HTTPS, SSH or
/// scp-like form.
pub fn github_repo(url: &str) -> Option<String> {
    let path = [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}").to_lowercase())
}

/// The repository's own file at `path`, from `origin/main`, or from the main
/// checkout until `origin/main` exists.
pub fn tracked(cwd: &Path, path: &str) -> Result<Option<String>, Failure> {
    Source::find(cwd)?.read(path)
}

impl Policy {
    /// Reads the policy for the repository containing `cwd` from Orchestral's
    /// config directory.
    pub fn load(cwd: &Path) -> Result<Policy, Failure> {
        Policy::load_from(&config_dir()?, cwd)
    }

    /// Reads the policy for the repository containing `cwd` from `config`.
    pub fn load_from(config: &Path, cwd: &Path) -> Result<Policy, Failure> {
        let files = Files::for_repo(config, cwd)?;
        let target = files.target();
        // Where `init` would move it, so the repository's own rules and
        // limits are never silently dropped.
        if !target.exists() && tracked(cwd, TRACKED_PATH)?.is_some() {
            return Err(Failure::Error(format!(
                "the policy is still in the repository's {TRACKED_PATH}, which isn't read; run `orchestral init` to move it to {}",
                target.display()
            )));
        }
        let sources = files.existing();
        if sources.is_empty() {
            return Err(Failure::Error(format!(
                "there's no policy; run `orchestral init` to create {}",
                target.display()
            )));
        }
        let mut texts = Vec::new();
        for path in &sources {
            let text = std::fs::read_to_string(path)
                .map_err(|err| Failure::Error(format!("{}: {err}", path.display())))?;
            texts.push((path.display().to_string(), text));
        }
        let has_contributing = tracked(cwd, "CONTRIBUTING.md")?.is_some();
        let layers: Vec<(&str, &str)> = texts
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        let mut policy = Policy::layered(&layers, has_contributing)?;
        policy.sources = sources;
        Ok(policy)
    }

    /// Parses one policy file. `has_contributing` sets the default for
    /// `supervisor_merges`.
    pub fn parse(text: &str, has_contributing: bool) -> Result<Policy, Failure> {
        Policy::layered(&[("the policy", text)], has_contributing)
    }

    /// Parses policy files, each named for its errors, with later ones
    /// overriding earlier ones' settings key by key and every file's rules
    /// applying in order.
    fn layered(files: &[(&str, &str)], has_contributing: bool) -> Result<Policy, Failure> {
        let mut layers = Vec::new();
        for (name, text) in files {
            layers.push(Layer::parse(name, text)?);
        }
        let merged = layers
            .into_iter()
            .reduce(|base, top| top.over(base))
            .expect("a policy has at least one file");
        let name = match files {
            [(name, _)] => *name,
            _ => "the policy",
        };
        let invalid = |message: &str| Failure::Error(format!("{name} is invalid: {message}"));
        let front = merged.front;
        let fleet_token_budget = match front.fleet_token_budget {
            Some(budget) if budget > 0 => budget,
            Some(_) => return Err(invalid("`fleet_token_budget` has to be above 0")),
            None => return Err(invalid("`fleet_token_budget` is required")),
        };
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
            preview: merged.preview,
            review_command: front.review_command,
        };
        Ok(Policy {
            settings,
            rules: merged.rules,
            sources: Vec::new(),
        })
    }
}

impl Layer {
    fn parse(name: &str, text: &str) -> Result<Layer, Failure> {
        let invalid = |message: String| Failure::Error(format!("{name} is invalid: {message}"));
        // Editors may add a byte order mark or Windows line endings.
        let text = text
            .strip_prefix('\u{feff}')
            .unwrap_or(text)
            .replace("\r\n", "\n");
        let (yaml, rules) = split_frontmatter(&text).ok_or_else(|| {
            invalid("it has to start with frontmatter between `---` lines".into())
        })?;
        let mut front: Frontmatter =
            yaml_serde::from_str(yaml).map_err(|err| invalid(err.to_string()))?;
        let preview = front
            .preview
            .take()
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
        Ok(Layer {
            front,
            preview,
            rules: rules.trim().to_owned(),
        })
    }

    /// This layer's settings over `base`'s, key by key, with `preview` as a
    /// whole, and `base`'s rules before this one's.
    fn over(self, base: Layer) -> Layer {
        let (top, base_front) = (self.front, base.front);
        let rules = [base.rules, self.rules]
            .into_iter()
            .filter(|rules| !rules.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        Layer {
            front: Frontmatter {
                workers: top.workers.or(base_front.workers),
                plan_limit_percent: top.plan_limit_percent.or(base_front.plan_limit_percent),
                fleet_token_budget: top.fleet_token_budget.or(base_front.fleet_token_budget),
                default_model: top.default_model.or(base_front.default_model),
                default_effort: top.default_effort.or(base_front.default_effort),
                merge_method: top.merge_method.or(base_front.merge_method),
                supervisor_merges: top.supervisor_merges.or(base_front.supervisor_merges),
                design_variants: top.design_variants.or(base_front.design_variants),
                nag_minutes: top.nag_minutes.or(base_front.nag_minutes),
                preview: None,
                review_command: top.review_command.or(base_front.review_command),
            },
            preview: self.preview.or(base.preview),
            rules,
        }
    }
}

impl std::fmt::Display for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let sources: Vec<String> = self
            .sources
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        writeln!(f, "From: {}", sources.join(", "))?;
        let yaml = yaml_serde::to_string(&self.settings).map_err(|_| std::fmt::Error)?;
        write!(f, "---\n{yaml}---")?;
        if !self.rules.is_empty() {
            write!(f, "\n{}", self.rules)?;
        }
        Ok(())
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

/// Where repository files are read from.
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
}

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
review_command: Run the review checklist.
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
        assert_eq!(
            settings.review_command.as_deref(),
            Some("Run the review checklist.")
        );
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

    #[test]
    fn a_repository_file_overrides_the_global_one_key_by_key() {
        let global = "---\nfleet_token_budget: 1000\nworkers: 2\npreview:\n  run: pnpm dev\n---\nAsk me first.\n";
        let repo = "---\nworkers: 5\npreview:\n  url: https://pr-{pr}.example.com\n---\nKeep the shim small.\n";

        let policy = Policy::layered(&[("global", global), ("repo", repo)], false).unwrap();

        assert_eq!(policy.settings.fleet_token_budget, 1000);
        assert_eq!(policy.settings.workers, 5);
        assert_eq!(
            policy.settings.preview.unwrap().start,
            PreviewStart::Url("https://pr-{pr}.example.com".into())
        );
        assert_eq!(policy.rules, "Ask me first.\n\nKeep the shim small.");
    }

    #[test]
    fn either_file_alone_is_enough_but_the_merged_result_needs_a_budget() {
        let repo_only = "---\nfleet_token_budget: 7\n---\n";
        assert_eq!(
            Policy::layered(&[("repo", repo_only)], false)
                .unwrap()
                .settings
                .fleet_token_budget,
            7
        );

        let Err(Failure::Error(message)) = Policy::layered(
            &[
                ("global", "---\nworkers: 1\n---\n"),
                ("repo", "---\nworkers: 2\n---\n"),
            ],
            false,
        ) else {
            panic!("expected an error");
        };
        assert_eq!(
            message,
            "the policy is invalid: `fleet_token_budget` is required"
        );
    }

    #[test]
    fn each_file_is_checked_on_its_own() {
        let Err(Failure::Error(message)) = Policy::layered(
            &[
                ("/c/policy.md", "---\nfleet_token_budget: 1\n---\n"),
                ("/c/a/b/policy.md", "---\nworker: 2\n---\n"),
            ],
            false,
        ) else {
            panic!("expected an error");
        };
        assert!(
            message.starts_with("/c/a/b/policy.md is invalid: unknown field `worker`"),
            "{message}"
        );
    }

    #[test]
    fn github_remotes_give_their_repository() {
        for url in [
            "https://github.com/Acme/App.git",
            "https://github.com/acme/app",
            "https://github.com/acme/app/",
            "git@github.com:acme/app.git",
            "ssh://git@github.com/acme/app.git",
        ] {
            assert_eq!(github_repo(url).as_deref(), Some("acme/app"), "{url}");
        }
        for url in [
            "",
            "/tmp/origin.git",
            "https://gitlab.com/acme/app.git",
            "https://github.com/acme",
            "https://github.com/acme/app/extra",
            "https://github.com/../app",
        ] {
            assert_eq!(github_repo(url), None, "{url}");
        }
    }
}
