//! The reporting window and the `--since-last` marker.
//!
//! Everything here runs against a real state directory in `/tmp`, pointed at
//! through `HERDR_PLUGIN_STATE_DIR`, and a real throwaway git repository. The
//! window is the one part of the digest where being wrong is invisible: a
//! mis-resolved `--since` renders as a beautifully formatted quiet day, so the
//! assertions below are mostly about *failing loudly* rather than about
//! producing a value.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use standup::clock;
use standup::config::{self, Config};
use standup::git::Git;
use standup::model::{Note, Severity, Stamp, WindowSource};
use standup::window;

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
    // And recording is refused rather than silently losing the window.
    window::record_run(&clock::stamp(clock::now()))
        .expect_err("a directory cannot be replaced by a file");
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
    assert_eq!(
        window.source,
        WindowSource::SinceLast {
            previous_run: Stamp {
                epoch: previous.epoch,
                local: previous.local.clone(),
                zone: previous.zone.clone(),
            }
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

#[test]
fn a_window_that_starts_now_is_flagged_instead_of_rendering_as_a_quiet_day() {
    let state = StateDir::new();
    let anchor = Anchor::new();

    let (window, notes) = window::resolve(
        &git(),
        &anchor.anchors(),
        &date_ref_repo(&state),
        &open_ended("today"),
    )
    .expect("git parses `today`, so this is a warning and not an error");

    assert_eq!(
        window.source,
        WindowSource::Explicit {
            spec: "today".to_string()
        }
    );
    // It really did land on now, which is the whole problem.
    assert!(
        (clock::now() - window.since.epoch).abs() <= 5,
        "`today` should resolve to the current instant, got {}",
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
