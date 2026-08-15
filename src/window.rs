//! Resolving the reporting window, and the `--since-last` marker.
//!
//! Windows are resolved to **absolute instants before any `git log` runs**, for
//! one reason: `git rev-parse --since=<garbage>` exits 0 and answers *now*, so
//! an unparseable `--since` would otherwise produce a perfectly formatted empty
//! digest. Resolving eagerly turns that into a loud error, and as a bonus lets
//! the header state exactly which instant the window starts at instead of
//! echoing the user's fuzzy phrase back at them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::clock;
use crate::config::{self, Config};
use crate::git::Git;
use crate::model::{Note, Stamp, Window, WindowSource};
use crate::Result;

/// How close to the current instant a resolved window start has to be before it
/// is worth saying out loud that the window is empty by construction. Generous
/// compared with the parse check in `git::resolve_date`, because this one only
/// adds a note.
const NOW_WINDOW_SLACK_SECONDS: i64 = 5;

/// Resolves the window the digest covers.
///
/// `anchors` are candidate directories to run `git rev-parse` inside, in
/// preference order; git refuses to parse a date outside a repository. If none
/// of them work, the caller's `date_ref_repo` is created and used, so the window
/// is still validated in a session with no checkouts at all.
///
/// Returns the window plus any notes worth showing the user — for example, that
/// `--since-last` found no previous run and fell back to the default.
pub fn resolve(
    git: &Git,
    anchors: &[PathBuf],
    date_ref_repo: &Path,
    config: &Config,
) -> Result<(Window, Vec<Note>)> {
    if config.since.trim().is_empty() {
        return Err("--since needs a value; an empty window start cannot be resolved".into());
    }

    let mut notes: Vec<Note> = Vec::new();
    let now = clock::now();

    // Where the start comes from. Only `--since-last` with a marker on record
    // arrives as an instant already; everything else is a spec git must parse.
    let (source, recorded_start) = if config.since_last {
        match previous_run() {
            Some(previous) => {
                // A marker ahead of the clock means the clock moved backwards or
                // the state directory came out of a backup. Left alone it would
                // render as a flawless empty digest, so it is refused by name.
                if previous.epoch > now {
                    return Err(format!(
                        "the recorded previous run is in the future: {} , but it is now {}. \
                         The clock moved, or {} was restored from a backup. \
                         Delete that file, or pass an explicit --since, and run again.",
                        previous.full(),
                        clock::stamp(now).full(),
                        config::last_run_file().display(),
                    )
                    .into());
                }
                let start = previous.epoch;
                (
                    WindowSource::SinceLast {
                        previous_run: previous,
                    },
                    Some(start),
                )
            }
            None => {
                // "You asked for today" and "this is the first run, so I fell
                // back to today" must not look the same.
                notes.push(Note::info(format!(
                    "--since-last has no previous run on record, so this digest covers \
                     the default window ({}) instead. The next --since-last will start \
                     where this run ends.",
                    config.since
                )));
                (WindowSource::SinceLastFirstRun, None)
            }
        }
    } else if config.since_is_explicit {
        (
            WindowSource::Explicit {
                spec: config.since.clone(),
            },
            None,
        )
    } else {
        (WindowSource::Default, None)
    };

    // Resolved through git's own approxidate parser, which rejects garbage
    // loudly. Letting that error propagate is the entire point of this module.
    let mut context: Option<PathBuf> = None;
    let since = match recorded_start {
        Some(epoch) => epoch,
        None => {
            let repo = parsing_context(git, anchors, date_ref_repo, &mut context)?;
            git.resolve_date(&repo, &config.since)?
        }
    };
    let until = match &config.until {
        Some(spec) => {
            let repo = parsing_context(git, anchors, date_ref_repo, &mut context)?;
            Some(git.resolve_date(&repo, spec)?)
        }
        None => None,
    };

    let since = clock::stamp(since);
    let until = until.map(clock::stamp);

    // A window that begins at the current instant can only ever be empty, which
    // is indistinguishable from a quiet day. Worth saying — but the *reason* it
    // happened differs by source, and advice about a word the user never typed
    // is worse than no advice.
    //
    // The common cause is `--since today`: git reads `today` as the current
    // instant rather than as the start of the day, and `midnight` is the
    // spelling that means 00:00 local. git accepts `today`, so `resolve_date`
    // cannot refuse it, which is exactly why it has to be caught here.
    if until.is_none() && (now - since.epoch).abs() <= NOW_WINDOW_SLACK_SECONDS {
        let advice = match &source {
            WindowSource::SinceLast { .. } => {
                "the previous digest was only moments ago, so there has been no time for \
                 anything to land. Pass an explicit --since to look further back."
            }
            _ => {
                "git reads \"today\" as the current instant rather than as the start of the \
                 day — try --since midnight, or --since \"12 hours ago\"."
            }
        };
        notes.push(Note::warning(format!(
            "this window starts at {}, which is now, so nothing can fall inside it. {advice}",
            since.full()
        )));
    }

    // A backwards window can only ever report nothing, and an empty digest is
    // exactly what a quiet day looks like. Say it out loud instead.
    if let Some(end) = &until {
        if since.epoch > end.epoch {
            return Err(format!(
                "the window starts after it ends: --since resolves to {} and --until to {}. \
                 Nothing can be reported over a window that runs backwards.",
                since.full(),
                end.full(),
            )
            .into());
        }
    }

    Ok((
        Window {
            since,
            until,
            source,
        },
        notes,
    ))
}

/// A directory `git rev-parse` can run in. The first anchor that is really
/// there wins; failing that, the throwaway bare repository is created, so a
/// session with no checkouts still gets its window validated rather than
/// silently trusted.
///
/// Memoised through `cache` because `--since` and `--until` both need one and
/// creating the fallback repository twice would be wasteful noise.
fn parsing_context(
    git: &Git,
    anchors: &[PathBuf],
    date_ref_repo: &Path,
    cache: &mut Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(context) = cache {
        return Ok(context.clone());
    }
    let context = match anchors.iter().find(|path| path.is_dir()) {
        Some(anchor) => anchor.clone(),
        None => {
            git.ensure_date_ref_repo(date_ref_repo)?;
            date_ref_repo.to_path_buf()
        }
    };
    *cache = Some(context.clone());
    Ok(context)
}

// ---------------------------------------------------------------------------
// The `--since-last` marker
// ---------------------------------------------------------------------------

/// On-disk form of the marker. Only `epoch` is load-bearing; `local` and `zone`
/// are written so the file can be read by a human, and are re-derived on read
/// so a marker written in one zone renders correctly in another.
#[derive(serde::Deserialize)]
struct Marker {
    epoch: i64,
}

/// The timestamp of the previous run a human read, if one was recorded.
///
/// A missing marker is the normal first-run case. A corrupt one is a warning
/// and a `None`, never a hard failure: a mangled state file must not stop the
/// digest from printing.
pub fn previous_run() -> Option<Stamp> {
    let path = config::last_run_file();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("standup: could not read {}: {err}", path.display());
            }
            return None;
        }
    };
    match serde_json::from_str::<Marker>(&raw) {
        // Re-stamped rather than trusted verbatim, so the rendering is in the
        // zone the reader is in now.
        Ok(marker) => Some(clock::stamp(marker.epoch)),
        Err(err) => {
            eprintln!(
                "standup: ignoring the unreadable last-run marker {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// Records this run's timestamp for a later `--since-last`.
///
/// Best-effort: an unwritable state directory is reported to stderr and
/// otherwise ignored, because it must not fail the digest the user asked for.
/// Written **after** a successful render, so a run that blew up does not
/// advance the marker past work it never showed anybody.
pub fn record_run(at: &Stamp) -> Result<()> {
    let path = config::last_run_file();
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|err| {
        format!(
            "could not create the state directory {}: {err}",
            dir.display()
        )
    })?;

    let mut body = serde_json::to_string_pretty(at)
        .map_err(|err| format!("could not encode the last-run marker: {err}"))?;
    body.push('\n');

    // Written to a temporary file beside the target and renamed over it. An
    // interrupted run must not leave half a marker behind, because the next
    // `--since-last` would refuse to parse it and silently fall back to the
    // default window.
    let temp = dir.join(format!(
        ".last-run.{}.{}.tmp",
        std::process::id(),
        next_temp_id()
    ));
    if let Err(err) = write_all(&temp, body.as_bytes()) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not write {}: {err}", temp.display()).into());
    }
    if let Err(err) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not replace {}: {err}", path.display()).into());
    }
    Ok(())
}

fn write_all(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    // The rename is only atomic with respect to a crash if the bytes are on
    // disk before it happens.
    file.sync_all()
}

/// Distinguishes the temporary files of two runs in one process, so a digest
/// that records twice cannot have one write clobber the other's temporary.
fn next_temp_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
