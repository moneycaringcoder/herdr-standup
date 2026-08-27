//! standup — a daily digest of what your agents actually did.
//!
//! Verb dispatch only; every verb is implemented in the library crate.

use standup::{clock, compare, config, render, standup as digest, window, Result};

const USAGE: &str = "\
standup — a daily digest of what your agents actually did

Usage: standup [VERB] [OPTIONS]

Verbs:
  --report            Human-readable digest (default)
  --markdown          The same digest as Markdown, ready to paste
  --slack             The same digest as Slack mrkdwn, which is not Markdown
  --html              The same digest as an email-ready HTML document
  --json              The same digest as JSON, for scripting
  --version           Print version and exit
  --help, -h          Show this help
  --format <NAME>     Select text, markdown, slack, html, or json

Window:
  --since <WHEN>      Anything git accepts: yesterday, '2 days ago', '09:00',
                      2026-08-01 (default: midnight, meaning local midnight).
                      Note that git reads 'today' as the current instant, not
                      as the start of the day — 'midnight' is the one you want.
  --until <WHEN>      End of the window (default: now)
  --since-last        Start from the last digest you read. Falls back to the
                      default window, loudly, the first time.
  --weekly            This ISO week, Monday to now, aggregated rather than
                      listed. Sets its own window.
  --monthly           This calendar month, the 1st to now, likewise.
  --diff <FILE>       Compare a digest saved by an earlier --json run with the
                      one this run collects. Reads as a comparison — what is
                      new, what finished, what stalled — not as a longer digest.

Selection:
  --path <DIR>        Also report this checkout, whether or not herdr knows it
  --offline           Report only --path directories; never touch the socket
  --by-agent          Group by agent rather than by repository. Opt-in: it
                      interleaves unrelated projects, and a commit cannot be
                      split between two agents sharing a checkout, so those are
                      counted under each and the totals stop reconciling.
  --busy              Hide repositories with nothing in the window
  --all               Show them (the default)
  --no-siblings       Only checkouts a workspace is sitting in
  --max-commits <N>   Commits listed per checkout before the rest are summarised

Exit status:
  --fail-if-empty     Exit 2 when there is nothing to report, for cron and CI.
                      The digest still prints. 2 rather than 1, because 1 means
                      the run failed and a quiet day is not a failure.

standup never writes to a repository and makes no network calls.
";

/// Exit status when `--fail-if-empty` finds nothing to report.
///
/// **2, not 1.** A failure already exits 1, and cron cannot act on a status that
/// means either "nothing happened today" or "the digest is broken". The two are
/// opposite messages: one is a quiet day, the other needs somebody to look.
const EXIT_EMPTY: i32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Err(err) => {
            eprintln!("standup: {err}");
            std::process::exit(1);
        }
        Ok(code) if code != 0 => std::process::exit(code),
        Ok(_) => {}
    }
}

fn parse_arguments(args: &[String]) -> Result<config::Arguments<'_>> {
    config::Arguments::parse(args).map_err(|err| format!("{err}\n\n{USAGE}").into())
}

fn run(args: &[String]) -> Result<i32> {
    let parsed = parse_arguments(args)?;
    let verb = parsed.verb();
    match verb {
        config::Verb::Report
        | config::Verb::Markdown
        | config::Verb::Slack
        | config::Verb::Html
        | config::Verb::Json => {
            let mut config = config::load_with_parsed_args(&parsed)?;
            // The verb picks the format unless `--format` said otherwise; both
            // spellings exist because the manifest wants one action per format
            // and a shell user wants one flag.
            if parsed.value("--format").is_none() {
                config.format = match verb {
                    config::Verb::Markdown => config::Format::Markdown,
                    config::Verb::Slack => config::Format::Slack,
                    config::Verb::Html => config::Format::Html,
                    config::Verb::Json => config::Format::Json,
                    _ => config.format,
                };
            }
            // A JSON run is a script reading, not a human. Advancing the
            // `--since-last` marker there would silently steal the window out
            // from under the next human digest.
            config.record_run = config.format != config::Format::Json;

            let digest = digest::build(&config)?;

            // A comparison is a different report, not a longer digest, so it
            // gets its own renderer and never advances the marker: what changed
            // between two digests is not "a digest a human read".
            if let Some(earlier) = parsed.value("--diff") {
                let before = compare::read_digest(std::path::Path::new(earlier))?;
                let comparison = compare::compare(&before, &digest);
                print!("{}", render::render_comparison(&comparison, &config)?);
                // What this run would post is the comparison, so that is what
                // "empty" has to describe here. A comparison where nothing moved
                // is exactly the message not worth sending.
                return Ok(empty_status(&config, comparison.is_quiet()));
            }

            print!("{}", render::render(&digest, &config)?);

            if config.record_run {
                if let Err(err) = window::record_run(&clock::stamp(digest.generated_at.epoch)) {
                    eprintln!("standup: could not record this run for --since-last: {err}");
                }
            }
            Ok(empty_status(&config, digest.is_quiet()))
        }
        config::Verb::Version => {
            println!("standup {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        config::Verb::Help => {
            print!("{USAGE}");
            Ok(0)
        }
    }
}

/// The exit status for a run that has already printed its output.
///
/// The digest is printed either way. `--fail-if-empty` is about what a caller
/// does next, and suppressing the output would take away the one thing that
/// tells a person reading the cron mail why the run failed.
fn empty_status(config: &config::Config, quiet: bool) -> i32 {
    if config.fail_if_empty && quiet {
        EXIT_EMPTY
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use standup::config::{Arguments, Verb};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_valued_option_accepts_both_spellings_and_last_value_wins() {
        for name in [
            "--since",
            "--until",
            "--path",
            "--format",
            "--max-commits",
            "--diff",
        ] {
            let separate = vec![name.to_string(), "separate".to_string()];
            let parsed = Arguments::parse(&separate).expect(name);
            assert_eq!(parsed.value(name), Some("separate"), "{name}");

            let inline = vec![format!("{name}=inline")];
            let parsed = Arguments::parse(&inline).expect(name);
            assert_eq!(parsed.value(name), Some("inline"), "{name}");

            let repeated = vec![
                name.to_string(),
                "first".to_string(),
                format!("{name}=last"),
            ];
            let parsed = Arguments::parse(&repeated).expect(name);
            assert_eq!(parsed.value(name), Some("last"), "{name}");
        }
    }

    #[test]
    fn every_standalone_option_rejects_a_value() {
        for name in [
            "--since-last",
            "--weekly",
            "--monthly",
            "--by-agent",
            "--fail-if-empty",
            "--offline",
            "--busy",
            "--all",
            "--no-siblings",
        ] {
            let valid = args(&[name]);
            Arguments::parse(&valid).expect(name);

            let invalid = vec![format!("{name}=false")];
            let err = Arguments::parse(&invalid).expect_err(name);
            assert!(
                err.to_string().contains(name) && err.to_string().contains("does not take a value"),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn every_bare_valued_option_refuses_a_recognized_token_as_its_value() {
        for name in [
            "--since",
            "--until",
            "--path",
            "--format",
            "--max-commits",
            "--diff",
        ] {
            for recognized in ["--busy", "--json"] {
                let raw = args(&[name, recognized]);
                let err = Arguments::parse(&raw).expect_err(name);
                assert!(
                    err.to_string().contains(name) && err.to_string().contains("requires a value"),
                    "{name} before {recognized}: {err}"
                );
            }
        }
    }

    #[test]
    fn the_reported_silent_acceptance_cases_are_named_errors() {
        for (raw, expected) in [
            (&["--quiet"][..], "unknown argument `--quiet`"),
            (
                &["--offline=false"][..],
                "`--offline` does not take a value",
            ),
            (
                &["--offline", "--path", "--busy", "--json"][..],
                "`--path` requires a value",
            ),
        ] {
            let raw = args(raw);
            let err = Arguments::parse(&raw).expect_err(expected);
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn options_can_surround_the_verb_and_repeated_paths_survive() {
        let raw = args(&[
            "--since",
            "first",
            "--path",
            "/a",
            "--json",
            "--path=/b",
            "--since=last",
        ]);
        let parsed = Arguments::parse(&raw).expect("valid ordering");
        assert_eq!(parsed.verb(), Verb::Json);
        assert_eq!(parsed.value("--since"), Some("last"));

        let config = standup::config::load_with_parsed_args(&parsed).expect("config");
        assert_eq!(
            config.extra_paths,
            vec![
                std::path::PathBuf::from("/a"),
                std::path::PathBuf::from("/b"),
            ]
        );
    }

    #[test]
    fn an_inline_value_may_begin_with_dashes() {
        let raw = args(&["--path=--busy"]);
        let parsed = Arguments::parse(&raw).expect("inline path");
        assert_eq!(parsed.value("--path"), Some("--busy"));
    }

    #[test]
    fn defaults_unknown_arguments_and_second_verbs_remain_strict() {
        let none = args(&[]);
        assert_eq!(Arguments::parse(&none).unwrap().verb(), Verb::Report);

        for raw in [
            &["--markdown", "--offline --path /x"][..],
            &["--typo"][..],
            &["extra"][..],
        ] {
            let raw = args(raw);
            let err = Arguments::parse(&raw).expect_err("unknown argument");
            assert!(err.to_string().contains("unknown argument"), "{err}");
        }

        let two_verbs = args(&["--json", "--markdown"]);
        let err = Arguments::parse(&two_verbs).expect_err("second verb");
        assert!(err.to_string().contains("second verb"), "{err}");
    }

    #[test]
    fn help_bypasses_invalid_trailing_input() {
        let raw = args(&["--help", "--offline=false", "--typo", "--json"]);
        assert_eq!(Arguments::parse(&raw).expect("help").verb(), Verb::Help);
    }
}
