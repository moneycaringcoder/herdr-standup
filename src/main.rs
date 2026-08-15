//! standup — a daily digest of what your agents actually did.
//!
//! Verb dispatch only; every verb is implemented in the library crate.

use standup::{clock, config, render, standup as digest, window, Result};

const USAGE: &str = "\
standup — a daily digest of what your agents actually did

Usage: standup [VERB] [OPTIONS]

Verbs:
  --report            Human-readable digest (default)
  --markdown          The same digest as Markdown, ready to paste
  --json              The same digest as JSON, for scripting
  --version           Print version and exit
  --help              Show this help

Window:
  --since <WHEN>      Anything git accepts: today, yesterday, '2 days ago',
                      2026-08-01 (default: midnight, meaning local midnight)
  --until <WHEN>      End of the window (default: now)
  --since-last        Start from the last digest you read. Falls back to the
                      default window, loudly, the first time.

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
const VALUED: [&str; 5] = ["--since", "--until", "--path", "--format", "--max-commits"];

/// The verb is the first argument that is neither an option's name nor its
/// value, so `standup --since yesterday --markdown` works as readily as
/// `standup --markdown --since yesterday`. Ordering that matters is a papercut
/// nobody should have to learn.
fn verb_of(args: &[String]) -> &str {
    const FLAGS: [&str; 6] = [
        "--since-last",
        "--offline",
        "--busy",
        "--all",
        "--no-siblings",
        "--quiet",
    ];
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
    match verb {
        "--report" | "--markdown" | "--json" => {
            let mut config = config::load_with_args(args)?;
            // The verb picks the format unless `--format` said otherwise; both
            // spellings exist because the manifest wants one action per format
            // and a shell user wants one flag.
            if config::value_arg(args, "--format")?.is_none() {
                config.format = match verb {
                    "--markdown" => config::Format::Markdown,
                    "--json" => config::Format::Json,
                    _ => config.format,
                };
            }
            // A JSON run is a script reading, not a human. Advancing the
            // `--since-last` marker there would silently steal the window out
            // from under the next human digest.
            config.record_run = config.format != config::Format::Json;

            let digest = digest::build(&config)?;
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
}
