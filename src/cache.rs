//! A cache for the landing probes, keyed by the shas that determine them.
//!
//! # What is expensive, and why
//!
//! Answering "did it land?" for a branch that is *not* an ancestor of the trunk
//! costs a walk of the trunk range. `git cherry` walks it, and when that finds
//! nothing the patch-id comparison walks it again with `-p`: measured on a real
//! repository, `git log -p` over an 800-commit range is 140,260,725 bytes of
//! patch text and about two seconds, and the diff is buffered before being
//! handed to `patch-id`, so it is also a ~140 MB allocation.
//!
//! standup's own shape makes that the normal case rather than the exception.
//! `git::Git::report` runs once per checkout, and a session is many worktrees of
//! one repository — twenty workspaces on one stale trunk paid that walk twenty
//! times, serially, for a range that was identical every time.
//!
//! # Why a cache can be exact here
//!
//! Both probes are pure functions of shas. `git cherry <trunk> <head>` and the
//! patch ids of `<base>..<trunk>` depend on nothing but the commits in that
//! range and the diff options, which are pinned in
//! `git::PATCH_ID_DIFF_OPTIONS`. So:
//!
//! - the **answer** for one checkout is determined by (head sha, trunk ref name,
//!   trunk sha);
//! - the **trunk patch ids** are determined by (fork point sha, trunk sha), and
//!   that key is identical for every worktree branched from the same commit of a
//!   repository whose trunk has not moved.
//!
//! Anything that could change an answer moves a sha and so changes the key.
//! There is no expiry and no invalidation logic to get wrong: a checkout that
//! moved simply misses.
//!
//! [`VERSION`] is the exception, and it is the one that matters. If the probes
//! themselves change — a different diff option, a third probe, a different
//! reading of `cherry` — every stored answer was computed by code that no longer
//! exists, so the version is bumped and the file is discarded rather than
//! trusted. A cache that outlived the meaning of its own contents is the way
//! this feature turns into wrong numbers.
//!
//! # It is invisible
//!
//! A hit and a miss produce the same digest. Nothing here is reported, no note
//! is added, and every failure — an unreadable file, an unwritable state
//! directory, a corrupt entry — degrades to recomputing. The one thing a cache
//! must never do is make the answer depend on whether it was there.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::model::Landed;
use crate::state_file;

/// Bumped whenever the probes change, which discards every stored answer.
///
/// The stored value is the *output* of code that may not exist any more, and it
/// carries no record of how it was produced. Anything that changes what a probe
/// would answer for the same shas — the diff options, the commands, how their
/// output is read — must bump this.
pub const VERSION: u32 = 1;

/// How many landing answers to keep. One per checkout per trunk position, so a
/// few hundred covers a large session's history several times over.
const MAX_ANSWERS: usize = 512;

/// How many trunk ranges to keep. Each is the expensive one — a list of patch
/// ids, one per trunk commit — so this is deliberately small.
const MAX_RANGES: usize = 16;

/// Trunk ranges longer than this are computed and not stored. A cache file is a
/// convenience; growing it without bound on a repository with a hundred thousand
/// commits is not.
const MAX_IDS_PER_RANGE: usize = 20_000;

/// A patch id and the trunk commit carrying it.
pub type PatchId = (String, String);

pub struct Cache {
    /// `None` for an in-memory cache: it still spares a run its own repeated
    /// work, and it touches no disk. That is the default, so a caller has to ask
    /// for persistence rather than acquire it by accident — a test that read the
    /// developer's real state directory would be neither hermetic nor honest.
    path: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// This run's sequence number, stamped on every entry it uses so eviction
    /// can drop the least recently useful rather than an arbitrary one.
    run: u64,
    answers: BTreeMap<String, Entry<Landed>>,
    ranges: BTreeMap<String, Entry<Vec<PatchId>>>,
    dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry<T> {
    /// The run that last used this entry, not the run that wrote it: an answer
    /// re-used every day should outlive one written yesterday and never read.
    used: u64,
    value: T,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    runs: u64,
    answers: BTreeMap<String, Entry<Landed>>,
    ranges: BTreeMap<String, Entry<Vec<PatchId>>>,
}

impl Cache {
    /// A cache that remembers within one run and writes nothing.
    pub fn in_memory() -> Cache {
        Cache {
            path: None,
            state: Mutex::new(State {
                run: 1,
                ..State::default()
            }),
        }
    }

    /// The persistent cache, in the plugin's own state directory.
    pub fn load() -> Cache {
        Cache::at(crate::config::cache_file())
    }

    /// The persistent cache at an explicit path.
    ///
    /// Anything unreadable is treated as an empty cache. A cache that refused to
    /// run because of its own file would be worse than no cache at all.
    pub fn at(path: PathBuf) -> Cache {
        let stored = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Stored>(&raw).ok())
            .filter(|stored| stored.version == VERSION);
        let state = match stored {
            Some(stored) => State {
                run: stored.runs.saturating_add(1),
                answers: stored.answers,
                ranges: stored.ranges,
                dirty: false,
            },
            None => State {
                run: 1,
                ..State::default()
            },
        };
        Cache {
            path: Some(path),
            state: Mutex::new(state),
        }
    }

    /// The key for one checkout's landing answer.
    ///
    /// The trunk's *name* is in the key as well as its sha, because the answer
    /// carries it: `Merged { into }` names the branch it landed on, and a
    /// repository that gained an `origin/HEAD` pointing somewhere else has a
    /// different answer to report even if both names resolve to one commit.
    pub fn answer_key(head: &str, trunk_name: &str, trunk: &str) -> String {
        format!("{head} {trunk_name} {trunk}")
    }

    /// The key for the patch ids of a trunk range.
    pub fn range_key(base: &str, trunk: &str) -> String {
        format!("{base}..{trunk}")
    }

    pub fn answer(&self, key: &str) -> Option<Landed> {
        let mut state = self.state.lock().ok()?;
        let run = state.run;
        let entry = state.answers.get_mut(key)?;
        let touched = entry.used != run;
        entry.used = run;
        let value = entry.value.clone();
        // Recording the hit is what keeps an answer read every day from being
        // evicted by one written yesterday and never used again.
        state.dirty |= touched;
        Some(value)
    }

    pub fn remember_answer(&self, key: String, value: Landed) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let used = state.run;
        state.answers.insert(key, Entry { used, value });
        state.dirty = true;
    }

    pub fn range(&self, key: &str) -> Option<Vec<PatchId>> {
        let mut state = self.state.lock().ok()?;
        let run = state.run;
        let entry = state.ranges.get_mut(key)?;
        let touched = entry.used != run;
        entry.used = run;
        let value = entry.value.clone();
        state.dirty |= touched;
        Some(value)
    }

    pub fn remember_range(&self, key: String, value: Vec<PatchId>) {
        if value.len() > MAX_IDS_PER_RANGE {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let used = state.run;
        state.ranges.insert(key, Entry { used, value });
        state.dirty = true;
    }

    /// Writes the cache back, best-effort.
    ///
    /// Silent on failure by design: an unwritable state directory means the next
    /// run recomputes, which is the behaviour without a cache at all, and is not
    /// worth a line of a digest somebody asked for.
    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.dirty {
            return;
        }
        evict(&mut state.answers, MAX_ANSWERS);
        evict(&mut state.ranges, MAX_RANGES);
        let stored = Stored {
            version: VERSION,
            runs: state.run,
            answers: state.answers.clone(),
            ranges: state.ranges.clone(),
        };
        let Ok(mut body) = serde_json::to_string(&stored) else {
            return;
        };
        body.push('\n');
        if state_file::replace(path, "plumbing-cache", body.as_bytes()).is_ok() {
            state.dirty = false;
        }
    }
}

/// Keeps the `max` most recently used entries.
///
/// Ties are broken by key so two runs with the same contents write the same
/// file, which is what makes the file itself checkable by hand.
fn evict<T>(entries: &mut BTreeMap<String, Entry<T>>, max: usize) {
    if entries.len() <= max {
        return;
    }
    let mut ordered: Vec<(u64, String)> = entries
        .iter()
        .map(|(key, entry)| (entry.used, key.clone()))
        .collect();
    // Most recently used first, then by key.
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, key) in ordered.into_iter().skip(max) {
        entries.remove(&key);
    }
}
