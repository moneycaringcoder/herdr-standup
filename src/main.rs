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
  --help              Show this help

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
  --busy              Hide repositories with nothing in the window
  --all               Show them (the default)
  --no-siblings       Only checkouts a workspace is sitting in
  --max-commits <N>   Commits listed per checkout before the rest are summarised

standup never writes to a repository and makes no network calls.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(err) = run(&args) {
        eprintln!("standup: {err}");
        std::process::exit(1);
    }
}

/// Options that take a value, and so must never be mistaken for the verb.
const VALUED: [&str; 6] = [
    "--since",
    "--until",
    "--path",
    "--format",
    "--max-commits",
    "--diff",
];

/// Options that stand alone.
const FLAGS: [&str; 8] = [
    "--since-last",
    "--weekly",
    "--monthly",
    "--offline",
    "--busy",
    "--all",
    "--no-siblings",
    "--quiet",
];

/// Every verb, so an argument that is none of the above can be rejected.
const VERBS: [&str; 8] = [
    "--report",
    "--markdown",
    "--slack",
    "--html",
    "--json",
    "--version",
    "--help",
    "-h",
];

/// Rejects anything that is not a verb, an option, or an option's value.
///
/// Silently ignoring an argument is the same class of bug as everything else
/// this plugin guards against: `standup --markdown "--offline --path /x"` — one
/// quoted argument, easily produced by a shell that does not word-split —
/// otherwise runs happily against the live session and prints a digest that
/// answers a different question than the one asked, with nothing to notice.
fn check_arguments(args: &[String]) -> Result<()> {
    let mut skip_value = false;
    let mut verb_seen = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if VALUED.contains(&name) {
            skip_value = !arg.contains('=');
            continue;
        }
        if FLAGS.contains(&name) {
            continue;
        }
        if VERBS.contains(&arg.as_str()) {
            if verb_seen {
                return Err(format!("`{arg}` is a second verb; pass only one\n\n{USAGE}").into());
            }
            verb_seen = true;
            continue;
        }
        return Err(format!("unknown argument `{arg}`\n\n{USAGE}").into());
    }
    Ok(())
}

/// The verb is the first argument that is neither an option's name nor its
/// value, so `standup --since yesterday --markdown` works as readily as
/// `standup --markdown --since yesterday`. Ordering that matters is a papercut
/// nobody should have to learn.
fn verb_of(args: &[String]) -> &str {
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if VALUED.contains(&name) {
            // `--since=today` carries its value; bare `--since today` does not.
            skip_value = !arg.contains('=');
            continue;
        }
        if FLAGS.contains(&name) {
            continue;
        }
        return arg;
    }
    "--report"
}

fn run(args: &[String]) -> Result<()> {
    let verb = verb_of(args);
    // `--help` has to work even when the rest of the line is wrong; that is
    // usually why somebody is asking for it.
    if verb != "--help" && verb != "-h" {
        check_arguments(args)?;
    }
    match verb {
        "--report" | "--markdown" | "--slack" | "--html" | "--json" => {
            let mut config = config::load_with_args(args)?;
            // The verb picks the format unless `--format` said otherwise; both
            // spellings exist because the manifest wants one action per format
            // and a shell user wants one flag.
            if config::value_arg(args, "--format")?.is_none() {
                config.format = match verb {
                    "--markdown" => config::Format::Markdown,
                    "--slack" => config::Format::Slack,
                    "--html" => config::Format::Html,
                    "--json" => config::Format::Json,
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
            if let Some(earlier) = config::value_arg(args, "--diff")? {
                let before = compare::read_digest(std::path::Path::new(&earlier))?;
                let comparison = compare::compare(&before, &digest);
                print!("{}", render::render_comparison(&comparison, &config)?);
                return Ok(());
            }

            print!("{}", render::render(&digest, &config)?);

            if config.record_run {
                if let Err(err) = window::record_run(&clock::stamp(digest.generated_at.epoch)) {
                    eprintln!("standup: could not record this run for --since-last: {err}");
                }
            }
            Ok(())
        }
        "--version" => {
            println!("standup {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown verb `{other}`\n\n{USAGE}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::verb_of;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_verb_is_found_whatever_the_order() {
        assert_eq!(verb_of(&args(&["--json"])), "--json");
        assert_eq!(
            verb_of(&args(&["--since", "today", "--markdown"])),
            "--markdown"
        );
        assert_eq!(
            verb_of(&args(&["--markdown", "--since", "today"])),
            "--markdown"
        );
        assert_eq!(verb_of(&args(&["--since=today", "--json"])), "--json");
    }

    #[test]
    fn bare_flags_are_not_verbs() {
        assert_eq!(verb_of(&args(&["--since-last"])), "--report");
        assert_eq!(verb_of(&args(&["--busy", "--markdown"])), "--markdown");
        assert_eq!(verb_of(&args(&["--offline", "--path", "/tmp"])), "--report");
    }

    #[test]
    fn no_arguments_means_a_human_report() {
        assert_eq!(verb_of(&args(&[])), "--report");
    }

    #[test]
    fn an_option_value_is_never_mistaken_for_a_verb() {
        // A window spec that looks like a verb must still be treated as a value.
        assert_eq!(verb_of(&args(&["--since", "--json"])), "--report");
    }

    /// Found the hard way: a shell that does not word-split an unquoted
    /// variable hands the whole option string over as one argument. Ignoring it
    /// ran the digest against the live session instead of the paths asked for,
    /// and printed a perfectly plausible answer to a different question.
    #[test]
    fn an_argument_that_is_not_understood_is_refused() {
        let err = super::check_arguments(&args(&["--markdown", "--offline --path /x"]))
            .expect_err("a run-together option string must not be accepted");
        assert!(err.to_string().contains("unknown argument"), "{err}");

        assert!(super::check_arguments(&args(&["--typo"])).is_err());
        assert!(super::check_arguments(&args(&["extra"])).is_err());
        // A second verb is a mistake worth naming rather than silently ranking.
        assert!(super::check_arguments(&args(&["--json", "--markdown"])).is_err());
    }

    #[test]
    fn every_documented_spelling_is_accepted() {
        super::check_arguments(&args(&[
            "--markdown",
            "--offline",
            "--path",
            "/x",
            "--path=/y",
            "--since",
            "yesterday",
            "--until=now",
            "--busy",
            "--no-siblings",
            "--max-commits",
            "5",
        ]))
        .expect("the documented spellings must all be accepted");
        super::check_arguments(&args(&[])).expect("no arguments is the default report");
    }
}
