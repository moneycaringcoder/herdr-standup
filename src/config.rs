//! Configuration, plugin identity, and the directories herdr hands us.
//!
//! Every other module reads this and none of them change it.

use std::path::PathBuf;
use std::time::Duration;

use crate::Result;

pub const PLUGIN_ID: &str = "moneycaringcoder.standup";

/// The default window: local midnight today. Given to git verbatim, and git
/// resolves `midnight` to 00:00 in the local zone.
pub const DEFAULT_SINCE: &str = "midnight";

/// Remote-tracking and local names tried, in order, when a repository has no
/// `refs/remotes/origin/HEAD` to point at its default branch.
pub const DEFAULT_BRANCH_CANDIDATES: &[&str] = &[
    "origin/main",
    "origin/master",
    "origin/trunk",
    "main",
    "master",
    "trunk",
];

/// Paths whose *line* counts are noise, excluded by default.
///
/// Lines added and removed are a proxy for effort, and one regenerated lockfile
/// destroys it: a `pnpm-lock.yaml` churns tens of thousands of lines that nobody
/// wrote. These are still counted as **files touched** — the commit really did
/// touch them — and contribute nothing to the line totals, which is exactly how
/// this module already treats a binary file.
///
/// Chosen to be the obvious cases and nothing clever. Each entry is a
/// [`Ignored`] pattern:
///
/// - dependency lockfiles, which are generated wholesale from a manifest a
///   human did write;
/// - vendored and installed dependency trees, which are somebody else's lines;
/// - build output that people commit anyway.
///
/// Deliberately absent: anything that guesses. No `*.json`, no `*.lock`, no
/// `generated` substring match, no minified-file heuristic. A wrong exclusion is
/// worse than a missing one, because it silently shrinks a real number.
pub const DEFAULT_IGNORED_PATHS: &[&str] = &[
    // Lockfiles.
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "go.sum",
    "flake.lock",
    "pubspec.lock",
    "Package.resolved",
    "gradle.lockfile",
    // Vendored and installed dependency trees.
    "vendor/",
    "node_modules/",
    "third_party/",
    ".yarn/",
    // Build output.
    "target/",
    "dist/",
    "build/",
    ".next/",
    ".svelte-kit/",
];

/// Decides whether a repository-relative path's lines are counted.
///
/// A deliberately small matcher rather than a glob dependency, with three shapes
/// and no others, so what it does can be held in the head:
///
/// | pattern | matches |
/// |---|---|
/// | `Cargo.lock` | any file with that **basename**, at any depth |
/// | `vendor/` | anything under a directory of that name, at any depth |
/// | `docs/api/*.json` | the whole path, where `*` stops at a `/` |
///
/// Basename matching for the bare form is what makes one `Cargo.lock` entry
/// cover a workspace with nine crates. Segment matching for the trailing-slash
/// form is what makes `node_modules/` cover `web/node_modules/…` as well as the
/// root. `*` never crosses a separator, so `dist/*` is one level and not a
/// subtree — the trailing-slash form is how a subtree is asked for.
///
/// An empty list matches nothing, which is how a reader turns the feature off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignored {
    patterns: Vec<String>,
}

impl Ignored {
    pub fn new(patterns: Vec<String>) -> Self {
        Self {
            patterns: patterns
                .into_iter()
                .map(|pattern| pattern.trim().to_string())
                .filter(|pattern| !pattern.is_empty())
                .collect(),
        }
    }

    /// Whether this path's lines are excluded from the totals.
    pub fn matches(&self, path: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| Ignored::matches_one(pattern, path))
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    fn matches_one(pattern: &str, path: &str) -> bool {
        if let Some(directory) = pattern.strip_suffix('/') {
            // Every segment except the last, so a *file* called `dist` is not
            // mistaken for a directory of build output.
            let Some((parents, _file)) = path.rsplit_once('/') else {
                return false;
            };
            return parents.split('/').any(|segment| segment == directory);
        }
        if pattern.contains('/') {
            return glob_segment(pattern, path);
        }
        let basename = path.rsplit('/').next().unwrap_or(path);
        glob_segment(pattern, basename)
    }
}

impl Default for Ignored {
    fn default() -> Self {
        Ignored::new(
            DEFAULT_IGNORED_PATHS
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
        )
    }
}

/// `*` matches any run of characters other than `/`; everything else is literal.
///
/// Written out rather than pulled in, because the whole grammar is one
/// wildcard and a dependency would be more code than this in the lockfile
/// alone — a lockfile this feature exists to stop counting.
fn glob_segment(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((prefix, rest)) => {
            if !text.starts_with(prefix) {
                return false;
            }
            let remainder = &text[prefix.len()..];
            // The wildcard stops at a separator, so a pattern for one directory
            // level never silently becomes a whole subtree.
            let stop = remainder.find('/').unwrap_or(remainder.len());
            (0..=stop).any(|split| glob_segment(rest, &remainder[split..]))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Window start, as a git approxidate string.
    pub since: String,
    /// Whether [`Config::since`] was asked for, by a `--since` on the command
    /// line or a `since` in the config file, rather than being the built-in
    /// default. Comparing the string against the default would get this wrong
    /// for somebody who explicitly asks for `midnight`, and the window header
    /// says something different in each case.
    pub since_is_explicit: bool,
    /// Window end. `None` means "up to now".
    pub until: Option<String>,
    /// Take the window start from the previous run's timestamp.
    pub since_last: bool,
    /// Record this run's timestamp for a later `--since-last`. A `--json` run
    /// piped into a script should not move the marker out from under the
    /// human's next `standup`, so only the verbs that a person reads set this.
    pub record_run: bool,
    pub format: Format,
    /// Commits listed per checkout before the rest are summarised as a count.
    /// A standup digest is read, not scrolled.
    pub max_commits: usize,
    /// Show repositories with nothing in the window. On by default: a quiet
    /// workspace summarised as quiet is information, an omitted one is a lie.
    pub include_quiet: bool,
    /// Also report checkouts of a repository that no workspace is sitting in —
    /// typically a worktree whose agent finished and whose workspace was
    /// closed. That work still came out of today, and it is exactly the work a
    /// per-workspace view would lose.
    pub include_siblings: bool,
    /// Timeout for any single git invocation, so one wedged repository cannot
    /// hang the digest.
    pub git_timeout: Duration,
    /// Extra checkout paths to include, beyond what herdr reports. Lets the
    /// plugin be useful from a shell, and lets the tests drive it.
    pub extra_paths: Vec<PathBuf>,
    /// Report the digest for these paths only, skipping the herdr socket
    /// entirely.
    pub offline: bool,
    /// Paths whose lines are not counted. Defaults to
    /// [`DEFAULT_IGNORED_PATHS`]; a list in the config file replaces it
    /// wholesale rather than adding to it, so a reader can both extend the
    /// defaults and get rid of them.
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Json,
}

impl Format {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" | "plain" => Ok(Format::Text),
            "markdown" | "md" => Ok(Format::Markdown),
            "json" => Ok(Format::Json),
            other => {
                Err(format!("unknown --format `{other}`; expected text, markdown or json").into())
            }
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            since: DEFAULT_SINCE.to_string(),
            since_is_explicit: false,
            until: None,
            since_last: false,
            record_run: false,
            format: Format::Text,
            max_commits: 10,
            include_quiet: true,
            include_siblings: true,
            git_timeout: Duration::from_secs(20),
            extra_paths: Vec::new(),
            offline: false,
            ignore: DEFAULT_IGNORED_PATHS
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
        }
    }
}

pub fn load_with_args(args: &[String]) -> Result<Config> {
    let mut config = load_file();

    if let Some(since) = value_arg(args, "--since")? {
        config.since = since;
        config.since_is_explicit = true;
    }
    if let Some(until) = value_arg(args, "--until")? {
        config.until = Some(until);
    }
    if let Some(raw) = value_arg(args, "--format")? {
        config.format = Format::parse(&raw)?;
    }
    if let Some(raw) = value_arg(args, "--max-commits")? {
        config.max_commits = raw
            .trim()
            .parse::<usize>()
            .map_err(|err| format!("--max-commits {raw}: {err}"))?;
    }
    for path in all_value_args(args, "--path")? {
        config.extra_paths.push(PathBuf::from(path));
    }
    if args.iter().any(|a| a == "--since-last") {
        config.since_last = true;
    }
    if args.iter().any(|a| a == "--all") {
        config.include_quiet = true;
    }
    if args.iter().any(|a| a == "--busy") {
        config.include_quiet = false;
    }
    if args.iter().any(|a| a == "--offline") {
        config.offline = true;
    }
    if args.iter().any(|a| a == "--no-siblings") {
        config.include_siblings = false;
    }

    // `--since` and `--since-last` answer the same question differently, and
    // silently preferring one would make the other look broken.
    if config.since_last && value_arg(args, "--since")?.is_some() {
        return Err("--since and --since-last are mutually exclusive".into());
    }
    Ok(config)
}

/// On-disk form. Every field optional, unknown keys ignored, so a newer file
/// does not break an older binary.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct FileConfig {
    since: Option<String>,
    format: Option<String>,
    max_commits: Option<usize>,
    include_quiet: Option<bool>,
    git_timeout_seconds: Option<u64>,
    ignore: Option<Vec<String>>,
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// Reads the config file over the defaults. A missing file is normal; a
/// malformed one is a warning and the defaults, never a hard failure.
fn load_file() -> Config {
    let path = config_file();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("standup: ignoring {}: {err}", path.display());
            }
            return Config::default();
        }
    };
    let file: FileConfig = match serde_json::from_str(&raw) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("standup: ignoring malformed {}: {err}", path.display());
            return Config::default();
        }
    };

    let mut config = Config::default();
    if let Some(since) = file.since.filter(|s| !s.trim().is_empty()) {
        config.since = since;
        config.since_is_explicit = true;
    }
    if let Some(raw) = file.format {
        match Format::parse(&raw) {
            Ok(format) => config.format = format,
            Err(err) => eprintln!("standup: ignoring format in {}: {err}", path.display()),
        }
    }
    if let Some(max) = file.max_commits {
        config.max_commits = max;
    }
    if let Some(quiet) = file.include_quiet {
        config.include_quiet = quiet;
    }
    if let Some(seconds) = file.git_timeout_seconds.filter(|s| *s > 0) {
        config.git_timeout = Duration::from_secs(seconds);
    }
    // An empty list is meaningful and is honoured: it turns the exclusion off,
    // which somebody who wants the raw numbers is entitled to ask for.
    if let Some(ignore) = file.ignore {
        config.ignore = ignore;
    }
    config
}

/// Value of `--name <VALUE>` or `--name=<VALUE>`, last occurrence winning.
///
/// A missing value the user typed is a hard error, unlike a malformed config
/// file: they are looking right at it, and silently ignoring it would be worse.
pub fn value_arg(args: &[String], name: &str) -> Result<Option<String>> {
    Ok(all_value_args(args, name)?.pop())
}

/// Every occurrence of `--name <VALUE>`, in order.
pub fn all_value_args(args: &[String], name: &str) -> Result<Vec<String>> {
    let flag = format!("{name}=");
    let mut found = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(value) = arg.strip_prefix(&flag) {
            found.push(value.to_string());
        } else if arg == name {
            found.push(rest.next().ok_or(format!("{name} needs a value"))?.clone());
        }
    }
    Ok(found)
}

pub fn plugin_id() -> String {
    non_empty_env("HERDR_PLUGIN_ID").unwrap_or_else(|| PLUGIN_ID.to_string())
}

/// Where the `--since-last` marker lives: `~/.local/state/herdr/plugins/<id>/`.
///
/// herdr injects `HERDR_PLUGIN_STATE_DIR` and is authoritative when it does, but
/// the fallback must resolve to the *same* directory. A fallback that pointed
/// somewhere else would give `--since-last` from a plugin action and
/// `--since-last` from a shell two different marker files, and the shell one
/// would silently report the wrong window forever.
pub fn state_dir() -> PathBuf {
    non_empty_env("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            xdg_dir("XDG_STATE_HOME", ".local/state")
                .join("herdr")
                .join("plugins")
                .join(plugin_id())
        })
}

/// Where the config file lives. Same split-brain rule as [`state_dir`].
pub fn config_dir() -> PathBuf {
    non_empty_env("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            xdg_dir("XDG_CONFIG_HOME", ".config")
                .join("herdr")
                .join("plugins")
                .join("config")
                .join(plugin_id())
        })
}

/// Marker holding the timestamp of the last run that a human read.
pub fn last_run_file() -> PathBuf {
    state_dir().join("last-run.json")
}

/// A throwaway repository used only to resolve `--since` strings through git's
/// own approxidate parser when the session has no checkouts of its own. Empty,
/// bare, and never written to by anything but `git init`.
pub fn date_ref_repo() -> PathBuf {
    state_dir().join("dateref.git")
}

/// An XDG base directory. An absolute variable wins; the spec says a relative
/// one must be ignored. The temp fallback is for a process with no home at all,
/// and is the wrong place for state — but it is better than the working
/// directory, which for this plugin is somebody's repository.
fn xdg_dir(variable: &str, relative: &str) -> PathBuf {
    if let Some(base) = non_empty_env(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return base;
    }
    match non_empty_env("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        Some(home) => home.join(relative),
        None => std::env::temp_dir().join("herdr-no-home"),
    }
}

/// herdr injects empty strings for absent context, so empty means unset.
pub fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// One `Cargo.lock` entry has to cover a workspace with nine crates, which
    /// is why the bare form matches the basename at any depth.
    #[test]
    fn a_bare_pattern_matches_the_basename_at_any_depth() {
        let ignored = Ignored::new(vec!["Cargo.lock".to_string()]);
        assert!(ignored.matches("Cargo.lock"));
        assert!(ignored.matches("crates/core/Cargo.lock"));
        assert!(!ignored.matches("Cargo.toml"));
        assert!(
            !ignored.matches("Cargo.lock.bak"),
            "the basename has to match, not merely start with the pattern"
        );
    }

    /// `node_modules/` has to cover `web/node_modules/…` as well as the root,
    /// because that is where it actually turns up.
    #[test]
    fn a_trailing_slash_matches_a_directory_at_any_depth() {
        let ignored = Ignored::new(vec!["node_modules/".to_string()]);
        assert!(ignored.matches("node_modules/react/index.js"));
        assert!(ignored.matches("web/node_modules/react/index.js"));
        assert!(!ignored.matches("src/node_modules.md"));
    }

    /// A *file* named like a build directory is not build output. The last
    /// segment is a filename and is never treated as a directory.
    #[test]
    fn a_file_named_like_a_directory_is_not_excluded() {
        let ignored = Ignored::new(vec!["dist/".to_string()]);
        assert!(ignored.matches("dist/app.js"));
        assert!(!ignored.matches("dist"));
        assert!(!ignored.matches("scripts/dist"));
    }

    #[test]
    fn a_wildcard_never_crosses_a_separator() {
        let ignored = Ignored::new(vec!["docs/*.json".to_string()]);
        assert!(ignored.matches("docs/api.json"));
        assert!(
            !ignored.matches("docs/v2/api.json"),
            "`*` is one level; a subtree is asked for with a trailing slash"
        );
        assert!(!ignored.matches("api.json"));
    }

    #[test]
    fn a_bare_wildcard_matches_the_basename_only() {
        let ignored = Ignored::new(vec!["*.min.js".to_string()]);
        assert!(ignored.matches("static/app.min.js"));
        assert!(!ignored.matches("static/app.js"));
    }

    /// Turning the exclusion off has to be possible, and an empty list is how.
    #[test]
    fn an_empty_list_matches_nothing() {
        let ignored = Ignored::new(Vec::new());
        assert!(!ignored.matches("Cargo.lock"));
        assert!(!ignored.matches("node_modules/react/index.js"));
    }

    /// The defaults are the point of the feature, so they are pinned rather than
    /// assumed: these are the paths a reader will actually meet.
    #[test]
    fn the_default_list_covers_the_obvious_cases() {
        let ignored = Ignored::default();
        for path in [
            "Cargo.lock",
            "crates/core/Cargo.lock",
            "pnpm-lock.yaml",
            "web/package-lock.json",
            "go.sum",
            "flake.lock",
            "vendor/github.com/pkg/errors/errors.go",
            "web/node_modules/react/index.js",
            "target/debug/build.rs",
            "dist/app.js",
            ".next/static/chunk.js",
        ] {
            assert!(ignored.matches(path), "{path} should not be counted");
        }
        for path in [
            "Cargo.toml",
            "src/main.rs",
            "package.json",
            "docs/target.md",
            "src/dist.rs",
            "vendor.rs",
        ] {
            assert!(!ignored.matches(path), "{path} is somebody's work");
        }
    }

    #[test]
    fn both_spellings_of_a_valued_flag_work() {
        let parsed = load_with_args(&args(&["--since=yesterday"])).unwrap();
        assert_eq!(parsed.since, "yesterday");
        let parsed = load_with_args(&args(&["--since", "2 days ago"])).unwrap();
        assert_eq!(parsed.since, "2 days ago");
    }

    /// Asking for `midnight` by name is not the same as not asking at all, even
    /// though the resulting window is identical — the header explains one and
    /// not the other.
    #[test]
    fn an_explicit_since_is_distinguishable_from_the_default() {
        assert!(!load_with_args(&args(&[])).unwrap().since_is_explicit);
        let parsed = load_with_args(&args(&["--since", DEFAULT_SINCE])).unwrap();
        assert_eq!(parsed.since, DEFAULT_SINCE);
        assert!(parsed.since_is_explicit);
    }

    #[test]
    fn a_valued_flag_with_no_value_is_an_error() {
        assert!(load_with_args(&args(&["--since"])).is_err());
    }

    #[test]
    fn contradictory_windows_are_refused_rather_than_ranked() {
        let err = load_with_args(&args(&["--since", "yesterday", "--since-last"])).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn formats_parse_by_name_and_alias() {
        assert_eq!(Format::parse("md").unwrap(), Format::Markdown);
        assert_eq!(Format::parse("JSON").unwrap(), Format::Json);
        assert!(Format::parse("yaml").is_err());
    }

    #[test]
    fn repeated_paths_all_survive() {
        let parsed = load_with_args(&args(&["--path", "/a", "--path=/b"])).unwrap();
        assert_eq!(
            parsed.extra_paths,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }
}
