//! The reporting window and the `--since-last` marker.
//!
//! Everything here runs against a real state directory in `/tmp`, pointed at
//! through `HERDR_PLUGIN_STATE_DIR`, and a real throwaway git repository. The
//! window is the one part of the digest where being wrong is invisible: a
//! mis-resolved `--since` renders as a beautifully formatted quiet day, so the
//! assertions below are mostly about *failing loudly* rather than about
//! producing a value.

#[path = "fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use standup::clock;
use standup::config::{self, Config};
use standup::git::Git;
use standup::model::{Note, Period, Severity, WindowSource};
use standup::{state_file, window};

/// `HERDR_PLUGIN_STATE_DIR` is process-global, so these tests run one at a
/// time even though cargo runs them on separate threads.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "standup-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ))
}

/// A private state directory, installed for the lifetime of one test.
struct StateDir {
    path: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl StateDir {
    fn new() -> Self {
        let guard = env_lock();
        let path = unique_dir("state");
        std::fs::create_dir_all(&path).expect("state dir");
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &path);
        Self {
            path,
            _guard: guard,
        }
    }

    /// A state directory that cannot be created, because a *file* sits where
    /// one of its parents would have to be.
    fn unwritable() -> Self {
        let guard = env_lock();
        let blocker = unique_dir("blocked");
        std::fs::write(&blocker, b"not a directory").expect("blocker");
        let path = blocker.join("state");
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &path);
        Self {
            path: blocker,
            _guard: guard,
        }
    }

    fn marker(&self) -> PathBuf {
        self.path.join("last-run.json")
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.path)
            .expect("read state dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        std::env::remove_var("HERDR_PLUGIN_STATE_DIR");
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An empty repository to parse dates in. `git rev-parse` refuses to run
/// outside one, which is the whole reason `resolve` takes anchors at all.
struct Anchor {
    path: PathBuf,
}

impl Anchor {
    fn new() -> Self {
        let path = unique_dir("anchor");
        std::fs::create_dir_all(&path).expect("anchor dir");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {}", path.display());
        Self { path }
    }

    fn anchors(&self) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn git() -> Git {
    Git::new(Duration::from_secs(20))
}

/// A `date_ref_repo` path inside the state directory, as the real caller uses.
fn date_ref_repo(state: &StateDir) -> PathBuf {
    state.path.join("dateref.git")
}

fn since_last_config() -> Config {
    Config {
        since_last: true,
        ..Config::default()
    }
}

fn write_marker(state: &StateDir, body: &str) {
    std::fs::write(state.marker(), body).expect("write marker");
}

// ---------------------------------------------------------------------------
// The marker on disk
// ---------------------------------------------------------------------------

#[test]
fn a_missing_marker_is_the_ordinary_first_run() {
    let state = StateDir::new();
    assert!(!state.marker().exists());

    assert_eq!(window::previous_run(), None);
}

#[test]
fn record_run_round_trips_through_previous_run() {
    let state = StateDir::new();
    let at = clock::stamp(1_786_831_294);

    window::record_run(&at).expect("record");

    assert!(state.marker().exists());
    let read_back = window::previous_run().expect("a marker was just written");
    assert_eq!(read_back.epoch, at.epoch);
    // The rendering is re-derived on read, so the file survives a zone change.
    assert_eq!(read_back.local, at.local);
    assert_eq!(read_back.zone, at.zone);
}

#[test]
fn record_run_keeps_an_equal_marker_byte_for_byte() {
    let state = StateDir::new();
    let raw = "{\n  \"epoch\": 1700000000,\n  \"local\": \"keep this exact marker\"\n}\n";
    write_marker(&state, raw);

    window::record_run(&clock::stamp(1_700_000_000)).expect("record equal epoch");

    assert_eq!(
        std::fs::read_to_string(state.marker()).expect("read marker"),
        raw
    );
    assert_eq!(state.entries(), vec!["last-run.json".to_string()]);
}

#[test]
fn record_run_keeps_a_newer_marker_byte_for_byte() {
    let state = StateDir::new();
    let raw = "{\n  \"epoch\": 1700000100,\n  \"local\": \"keep the newer marker\"\n}\n";
    write_marker(&state, raw);

    window::record_run(&clock::stamp(1_700_000_000)).expect("record older epoch");

    assert_eq!(
        std::fs::read_to_string(state.marker()).expect("read marker"),
        raw
    );
    assert_eq!(state.entries(), vec!["last-run.json".to_string()]);
}

#[test]
fn record_run_replaces_an_unreadable_marker() {
    let state = StateDir::new();
    write_marker(&state, "{\"epoch\": ");

    window::record_run(&clock::stamp(1_700_000_000)).expect("replace unreadable marker");

    assert_eq!(
        window::previous_run().map(|stamp| stamp.epoch),
        Some(1_700_000_000)
    );
    assert_eq!(state.entries(), vec!["last-run.json".to_string()]);
}

#[test]
fn a_blocked_older_writer_cannot_regress_a_newer_marker() {
    let state = StateDir::new();
    let older = clock::stamp(1_700_000_000);
    let newer = clock::stamp(1_700_000_100);
    let (older_started_tx, older_started_rx) = mpsc::channel();
    let (older_done_tx, older_done_rx) = mpsc::channel();
    let mut older_writer = None;

    // This is the newer writer's lock. It is acquired before the older writer
    // opens its own descriptor, so the channel schedule does not depend on
    // sleeps or on which waiter the kernel happens to wake first.
    state_file::with_directory_lock(&state.path, || {
        older_writer = Some(std::thread::spawn(move || {
            older_started_tx.send(()).expect("announce older writer");
            let result = window::record_run(&older).map_err(|err| err.to_string());
            older_done_tx.send(result).expect("return older result");
        }));
        older_started_rx.recv().expect("older writer started");

        let mut body = serde_json::to_string_pretty(&newer).expect("encode newer marker");
        body.push('\n');
        state_file::replace(&state.marker(), "last-run", body.as_bytes())
            .expect("newer writer commits while holding the lock");
        assert_eq!(
            older_done_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "the separately opened older writer must still be blocked"
        );
        Ok(())
    })
    .expect("newer writer lock");

    older_done_rx
        .recv()
        .expect("older writer returned")
        .expect("older writer succeeds without replacing newer state");
    older_writer
        .expect("older writer handle")
        .join()
        .expect("join older writer");

    assert_eq!(
        window::previous_run().map(|stamp| stamp.epoch),
        Some(newer.epoch)
    );
    assert_eq!(
        state.entries(),
        vec!["last-run.json".to_string()],
        "neither the advisory lock nor the atomic replacement leaves an artifact"
    );
}

#[test]
fn a_corrupt_marker_is_a_none_and_not_a_panic() {
    let state = StateDir::new();
    write_marker(&state, "{\"epoch\": ");

    assert_eq!(
        window::previous_run(),
        None,
        "a mangled state file must not stop the digest from printing"
    );
    // And it is still there afterwards: reading never repairs or deletes.
    assert!(state.marker().exists());
}

#[test]
fn a_marker_with_no_epoch_is_treated_as_corrupt() {
    let state = StateDir::new();
    write_marker(&state, "{\"local\": \"2026-08-15 09:12\"}");

    assert_eq!(window::previous_run(), None);
}

#[test]
fn record_run_replaces_the_marker_without_leaving_a_temporary_behind() {
    let state = StateDir::new();
    // A longer marker first, so a non-atomic overwrite would leave a tail of
    // the old file behind and the next parse would fail.
    write_marker(
        &state,
        &format!("{}\n{}", "{\"epoch\": 1}", " ".repeat(4_096)),
    );

    window::record_run(&clock::stamp(1_700_000_000)).expect("record");

    assert_eq!(
        state.entries(),
        vec!["last-run.json".to_string()],
        "the temporary file must be renamed, not left beside the target"
    );
    let raw = std::fs::read_to_string(state.marker()).expect("read");
    assert!(!raw.contains("    "), "old bytes survived: {raw:?}");
    assert_eq!(window::previous_run().map(|s| s.epoch), Some(1_700_000_000));
}

#[test]
fn record_run_creates_a_state_directory_that_is_not_there_yet() {
    let state = StateDir::new();
    std::fs::remove_dir_all(&state.path).expect("remove state dir");

    window::record_run(&clock::stamp(1_700_000_000)).expect("record");

    assert!(state.marker().exists());
}

#[test]
fn an_unwritable_state_directory_is_a_returned_error_not_a_panic() {
    let state = StateDir::unwritable();

    let err = window::record_run(&clock::stamp(1_700_000_000))
        .expect_err("a file sits where the directory would go");

    assert!(
        err.to_string().contains(&state.path.display().to_string()),
        "the message must name the path the caller has to fix: {err}"
    );
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[test]
fn the_default_window_is_labelled_as_the_default() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &Config::default(),
    )
    .expect("resolve");

    assert_eq!(window.source, WindowSource::Default);
    assert_eq!(window.until, None, "no --until means up to now");
    assert!(notes.is_empty());
    assert!(
        window.since.local.ends_with(" 00:00") || window.since.zone == "unknown zone",
        "the default window starts at local midnight: {}",
        window.since.full()
    );
}

#[test]
fn an_explicit_since_is_labelled_explicit_and_keeps_the_spec() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        since: "2026-01-02 09:00".to_string(),
        since_is_explicit: true,
        ..Config::default()
    };

    let (window, notes) =
        window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
            .expect("resolve");

    assert_eq!(
        window.source,
        WindowSource::Explicit {
            spec: "2026-01-02 09:00".to_string()
        }
    );
    assert!(notes.is_empty());
}

/// The same spec, asked for two different ways, is two different answers to
/// "why does this digest cover what it covers". The string cannot tell them
/// apart, which is what `Config::since_is_explicit` exists for.
#[test]
fn asking_for_midnight_is_explicit_even_though_midnight_is_the_default() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (defaulted, _) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &Config::default(),
    )
    .expect("resolve the default");

    let asked_for = Config {
        since: config::DEFAULT_SINCE.to_string(),
        since_is_explicit: true,
        ..Config::default()
    };
    let (explicit, _) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &asked_for,
    )
    .expect("resolve the explicit spelling");

    assert_eq!(defaulted.source, WindowSource::Default);
    assert_eq!(
        explicit.source,
        WindowSource::Explicit {
            spec: config::DEFAULT_SINCE.to_string()
        },
        "comparing the spec against the default would call this one Default too"
    );
    // Same instant either way: only the explanation differs.
    assert_eq!(defaulted.since.epoch, explicit.since.epoch);
}

#[test]
fn an_unparseable_since_is_refused_rather_than_answered_with_now() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        since: "gibberish".to_string(),
        ..Config::default()
    };

    // `git rev-parse --since=<garbage>` exits 0 and answers *now*. If that ever
    // gets through, the digest renders a typo as a quiet day.
    window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
        .expect_err("garbage must not resolve");
}

#[test]
fn the_first_run_of_since_last_falls_back_and_says_so_in_words() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    assert!(!state.marker().exists());

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &since_last_config(),
    )
    .expect("resolve");

    assert_eq!(
        window.source,
        WindowSource::SinceLastFirstRun,
        "\"you asked for today\" and \"I fell back to today\" must not look the same"
    );
    assert_eq!(notes.len(), 1, "the fallback has to be visible: {notes:?}");
    let message = notes[0].message.to_lowercase();
    assert!(message.contains("since-last"), "{message}");
    assert!(
        message.contains("no previous run"),
        "the note must say why: {message}"
    );
}

/// Found by attacking the built binary. Every unreadable marker fell back to
/// the default window and announced "no previous run on record" — word for word
/// what a genuine first run says. A first run is normal; this is a fault, it
/// will very likely repeat every day (`record_run` usually fails for the same
/// reason), and the only trace was a line on stderr that nobody reads in an
/// overlay pane.
#[test]
fn an_unreadable_marker_is_not_dressed_up_as_a_first_run() {
    for (label, body) in [
        ("zero bytes", ""),
        ("an array", "[]"),
        ("a bare string", "\"2026-08-01\""),
        ("epoch as an object", "{\"epoch\":{}}"),
        ("epoch as a string", "{\"epoch\":\"1786800000\"}"),
        ("truncated", "{\"epoch\": "),
    ] {
        let state = StateDir::new();
        let anchor = Anchor::new();
        write_marker(&state, body);

        let (window, notes) = window::resolve(
            &git(),
            &anchor.anchors(),
            &date_ref_repo(&state),
            &since_last_config(),
        )
        .unwrap_or_else(|err| panic!("{label} must not fail the digest: {err}"));

        // The window itself is the same fallback a first run gets — there is no
        // other honest answer, and `WindowSource` has no variant for this.
        assert_eq!(window.source, WindowSource::SinceLastFirstRun, "{label}");

        let warnings = warnings(&notes);
        assert_eq!(warnings.len(), 1, "{label}: {notes:?}");
        let message = &warnings[0].message;
        assert!(
            message.contains(&state.marker().display().to_string()),
            "{label}: the warning must name the file to delete: {message}"
        );
        assert!(
            !message.contains("no previous run on record"),
            "{label}: this is not a first run: {message}"
        );
    }
}

#[test]
fn a_marker_that_is_a_directory_is_unreadable_rather_than_fatal() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    std::fs::create_dir_all(state.marker()).expect("marker directory");

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &since_last_config(),
    )
    .expect("a directory where the marker goes must not fail the digest");

    assert_eq!(window.source, WindowSource::SinceLastFirstRun);
    assert_eq!(warnings(&notes).len(), 1, "{notes:?}");
    // Recording is refused rather than silently losing the window, and both
    // the temporary replacement and the advisory descriptor are cleaned up on
    // that error path.
    let at = clock::stamp(clock::now());
    window::record_run(&at).expect_err("a directory cannot be replaced by a file");
    assert_eq!(state.entries(), vec!["last-run.json".to_string()]);

    std::fs::remove_dir(state.marker()).expect("remove marker directory");
    window::record_run(&at).expect("the failed writer released the directory lock");
    assert_eq!(state.entries(), vec!["last-run.json".to_string()]);
}

/// A genuine first run keeps the calm wording: no warning, one plain note.
#[test]
fn a_genuine_first_run_is_still_only_an_info_note() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    assert!(!state.marker().exists());

    let (_, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &since_last_config(),
    )
    .expect("resolve");

    assert!(warnings(&notes).is_empty(), "{notes:?}");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].message.contains("no previous run on record"));
}

#[test]
fn a_recorded_marker_starts_the_window_where_the_last_run_ended() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let previous = clock::stamp(clock::now() - 3_600);
    window::record_run(&previous).expect("record");

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &since_last_config(),
    )
    .expect("resolve");

    assert_eq!(window.since.epoch, previous.epoch);
    // The whole stamp, not field by field: a marker that came back with a
    // different offset than the run that wrote it would be a different instant
    // wearing the same epoch.
    assert_eq!(
        window.source,
        WindowSource::SinceLast {
            previous_run: previous.clone()
        }
    );
    assert!(notes.is_empty(), "nothing to explain: {notes:?}");
}

#[test]
fn a_marker_in_the_future_is_refused_rather_than_reported_as_a_quiet_day() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    // A clock that moved, or a state directory restored from a backup.
    window::record_run(&clock::stamp(clock::now() + 86_400)).expect("record");

    let err = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &since_last_config(),
    )
    .expect_err("a window that starts tomorrow can only ever be empty");

    let message = err.to_string();
    assert!(message.contains("future"), "{message}");
    assert!(
        message.contains(&state.marker().display().to_string()),
        "the message must name the file to delete: {message}"
    );
}

#[test]
fn a_window_that_starts_after_it_ends_is_refused() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        since: "2026-01-02 00:00".to_string(),
        until: Some("2026-01-01 00:00".to_string()),
        ..Config::default()
    };

    let err = window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
        .expect_err("a backwards window reports nothing, which looks like a quiet day");

    assert!(err.to_string().contains("starts after it ends"), "{err}");
}

#[test]
fn an_until_that_parses_is_carried_into_the_window() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        since: "2026-01-01 00:00".to_string(),
        until: Some("2026-01-02 00:00".to_string()),
        ..Config::default()
    };

    let (window, _) = window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
        .expect("resolve");

    let until = window.until.expect("an --until was given");
    // A day apart, give or take whatever the host's zone does that night.
    let span = until.epoch - window.since.epoch;
    assert!(
        (82_800..=90_000).contains(&span),
        "one day apart, got {span}s"
    );
}

#[test]
fn a_session_with_no_checkouts_still_gets_its_window_validated() {
    let state = StateDir::new();
    let date_ref = date_ref_repo(&state);
    // No anchors at all, and one that does not exist, which is the same thing.
    let anchors = vec![PathBuf::from("/nonexistent/standup-anchor")];

    let (window, _) = window::resolve(&git(), &anchors, &date_ref, &Config::default())
        .expect("the fallback repository is created on demand");

    assert!(is_repo(&date_ref), "the date-ref repository must exist now");
    assert_eq!(window.source, WindowSource::Default);

    // And garbage is still refused, which is the point of having a context.
    let config = Config {
        since: "whenever".to_string(),
        ..Config::default()
    };
    window::resolve(&git(), &anchors, &date_ref, &config).expect_err("garbage must not resolve");
}

fn is_repo(path: &Path) -> bool {
    path.join("HEAD").exists() || path.join(".git").exists()
}

// ---------------------------------------------------------------------------
// Specs that legitimately mean "now"
//
// `git rev-parse --since=today` resolves to the current instant, not to the
// start of the day — `midnight` is the spelling that means 00:00 local. git
// parses `today` perfectly well, so `resolve_date` cannot refuse it, and the
// window it produces is empty by construction.
// ---------------------------------------------------------------------------

fn warnings(notes: &[Note]) -> Vec<&Note> {
    notes
        .iter()
        .filter(|note| note.severity == Severity::Warning)
        .collect()
}

fn open_ended(spec: &str) -> Config {
    Config {
        since: spec.to_string(),
        since_is_explicit: true,
        ..Config::default()
    }
}

/// A rollup window comes from the local clock, not from git's approxidate
/// parser, and lands on a calendar boundary. Asserted as properties rather than
/// against a fixed epoch, because the answer depends on the zone the test host
/// is in and on which day it is run — both of which are exactly what the
/// boundary has to be right about.
#[test]
fn a_weekly_rollup_starts_at_the_monday_of_this_week() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        rollup: Some(Period::Week),
        ..Config::default()
    };

    let (window, notes) =
        window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
            .expect("a rollup window needs no parsing and cannot fail on a spec");

    assert_eq!(
        window.source,
        WindowSource::Rollup {
            period: Period::Week
        }
    );
    assert_eq!(
        window.since.epoch,
        clock::week_start(clock::now()),
        "the window must be this week's Monday, not something git guessed"
    );
    assert!(window.until.is_none(), "a rollup runs up to now");
    assert!(notes.is_empty(), "nothing to explain: {notes:?}");
}

#[test]
fn a_monthly_rollup_starts_at_the_first_of_this_month() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    let config = Config {
        rollup: Some(Period::Month),
        ..Config::default()
    };

    let (window, notes) =
        window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
            .expect("a rollup window needs no parsing and cannot fail on a spec");

    assert_eq!(
        window.source,
        WindowSource::Rollup {
            period: Period::Month
        }
    );
    assert_eq!(window.since.epoch, clock::month_start(clock::now()));
    assert!(notes.is_empty(), "{notes:?}");
}

/// A rollup is a window in its own right, so it must not be mistaken for the
/// "empty by construction" case that `--since now` is warned about. The month
/// boundary is days in the past; nothing to warn about.
#[test]
fn a_rollup_is_never_flagged_as_starting_now() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    for period in [Period::Week, Period::Month] {
        let config = Config {
            rollup: Some(period),
            ..Config::default()
        };
        let (window, notes) =
            window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
                .expect("resolved");
        assert!(
            window.since.epoch <= clock::now(),
            "{:?} started in the future",
            period
        );
        assert!(
            notes.iter().all(|note| note.severity != Severity::Warning),
            "{:?} produced a warning: {notes:?}",
            period
        );
    }
}

/// Driven with `now`, whose meaning is the same on every git. This used to be
/// driven with `today`, which meant the same thing until git 2.55 changed it to
/// the local midnight — so the spec that pins the *mechanism* has to be the one
/// git has not changed its mind about. `today` gets its own test below.
#[test]
fn a_window_that_starts_now_is_flagged_instead_of_rendering_as_a_quiet_day() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &open_ended("now"),
    )
    .expect("git parses `now`, so this is a warning and not an error");

    assert_eq!(
        window.source,
        WindowSource::Explicit {
            spec: "now".to_string()
        }
    );
    // It really did land on now, which is the whole problem.
    assert!(
        (clock::now() - window.since.epoch).abs() <= 5,
        "`now` should resolve to the current instant, got {}",
        window.since.full()
    );

    let warnings = warnings(&notes);
    assert_eq!(
        warnings.len(),
        1,
        "an empty-by-construction window has to say so: {notes:?}"
    );
    assert!(
        warnings[0].message.contains("midnight"),
        "the warning must name the word the user wanted: {}",
        warnings[0].message
    );
}

/// `today` is the spec people actually type, and what it means depends on the
/// git in front of them: through 2.54 the current instant, from 2.55 the local
/// midnight. Both are accepted rather than refused, both land inside the current
/// local day, and the warning appears exactly when the window really is empty by
/// construction — which is to say on the older git and not on the newer one.
#[test]
fn today_is_accepted_whichever_instant_this_git_thinks_it_means() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &open_ended("today"),
    )
    .expect("git parses `today` on every version, so it is never an error");

    assert_eq!(
        window.source,
        WindowSource::Explicit {
            spec: "today".to_string()
        }
    );

    let now = clock::now();
    let started_now = (now - window.since.epoch).abs() <= 5;
    assert!(
        started_now || (window.since.epoch <= now && now - window.since.epoch < 86_400 + 3_600),
        "`today` landed outside the current local day: {}",
        window.since.full()
    );

    // Which branch below is live is a fact about this git, not a coin toss.
    // Pinning it is what keeps both halves honest: without this, a git that
    // changed its mind would quietly take the other branch and leave this one
    // untested — which is exactly what happened when every runner image
    // converged on 2.55.
    assert_eq!(
        started_now,
        fixtures::git_version() < (2, 55),
        "git {:?} read `today` as {}, which its version says it should not",
        fixtures::git_version(),
        if started_now { "now" } else { "midnight" }
    );

    let warnings = warnings(&notes);
    if started_now {
        // git through 2.54.
        assert_eq!(
            warnings.len(),
            1,
            "a window starting at the current instant has to say so: {notes:?}"
        );
        assert!(
            warnings[0].message.contains("midnight"),
            "the warning must name the word the user wanted: {}",
            warnings[0].message
        );
    } else {
        // git 2.55 and newer: an ordinary full-day window, nothing to warn about.
        assert!(
            warnings.is_empty(),
            "`today` meaning midnight is an ordinary window, not a problem: {notes:?}"
        );
    }
}

#[test]
fn an_ordinary_past_window_is_not_flagged() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (_, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &open_ended("12 hours ago"),
    )
    .expect("resolve");

    assert!(warnings(&notes).is_empty(), "{notes:?}");
}

#[test]
fn the_now_window_warning_is_only_for_an_open_ended_window() {
    let state = StateDir::new();
    let anchor = Anchor::new();
    // `--since today --until now` is a deliberate zero-width window, not a
    // mistake, so there is nothing to warn about.
    let config = Config {
        until: Some("now".to_string()),
        ..open_ended("today")
    };

    let (window, notes) =
        window::resolve(&git(), &anchor.anchors(), &date_ref_repo(&state), &config)
            .expect("resolve");

    assert!(window.until.is_some());
    assert!(warnings(&notes).is_empty(), "{notes:?}");
}
