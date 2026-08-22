//! The JSON is a promise, and this is where it is pinned.
//!
//! `--json` and `--diff --json` are documented as being for scripting, which
//! makes their shape an interface rather than an implementation detail. The
//! version field alone does not keep that promise: a version only means something
//! if it *moves* when the shape does, and nothing stops a field being renamed in
//! passing.
//!
//! So the shape is inventoried here — every path a consumer can see, and every
//! `kind` a tagged union can produce — against a literal list. Any change to the
//! JSON fails these tests, and the failure names the rule in
//! `docs/json-schema.md`: additive changes leave the version alone, breaking ones
//! bump it and say so in the changelog.
//!
//! The inventory is a union over a kitchen-sink digest that exercises every
//! variant, because a field that only appears on one enum arm is still part of
//! the interface.

use std::collections::BTreeSet;
use std::path::PathBuf;

use standup::compare;
use standup::model::{
    AgentRef, CheckoutDigest, CheckoutReport, Churn, Commit, Digest, Dirty, Equivalence, Head,
    Landed, Note, Period, RepoDigest, RepoKey, Stamp, Tracking, Unpushed, Window, WindowSource,
    WorkspaceRef, SCHEMA_VERSION,
};
use standup::render;

// ---------------------------------------------------------------------------
// Walking a document
// ---------------------------------------------------------------------------

fn kind_of(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Every `path: type` in the document, with array indices collapsed to `[]`.
///
/// A union: two array elements with different enum arms contribute both sets of
/// paths, which is what makes a field that only appears on one arm visible here.
fn walk(value: &serde_json::Value, prefix: &str, into: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, nested) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                into.insert(format!("{path}: {}", kind_of(nested)));
                walk(nested, &path, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let path = format!("{prefix}[]");
                into.insert(format!("{path}: {}", kind_of(item)));
                walk(item, &path, into);
            }
        }
        _ => {}
    }
}

fn inventory(json: &str) -> BTreeSet<String> {
    let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    let mut found = BTreeSet::new();
    walk(&parsed, "", &mut found);
    found
}

/// Every value of every `kind` field, which is how the tagged unions are read.
fn kinds(value: &serde_json::Value, into: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(kind)) = map.get("kind") {
                into.insert(kind.clone());
            }
            for nested in map.values() {
                kinds(nested, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                kinds(item, into);
            }
        }
        _ => {}
    }
}

fn all_kinds(json: &str) -> BTreeSet<String> {
    let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    let mut found = BTreeSet::new();
    kinds(&parsed, &mut found);
    found
}

/// What a failure has to tell whoever caused it.
const RULE: &str = "The JSON shape changed. Read docs/json-schema.md: an additive change updates \
                    this list and leaves SCHEMA_VERSION alone; a breaking one bumps the version, \
                    adds a changelog entry, and adds a row to the history table in that document.";

fn compare_sets(actual: &BTreeSet<String>, expected: &[&str], what: &str) {
    let expected: BTreeSet<String> = expected.iter().map(|s| s.to_string()).collect();
    let added: Vec<&String> = actual.difference(&expected).collect();
    let removed: Vec<&String> = expected.difference(actual).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "{what} moved.\n  added:   {added:?}\n  removed: {removed:?}\n\n{RULE}"
    );
}

// ---------------------------------------------------------------------------
// A digest with every variant in it
// ---------------------------------------------------------------------------

fn stamp(epoch: i64) -> Stamp {
    Stamp {
        epoch,
        local: "2026-08-15 09:12".to_string(),
        zone: "CEST +0200".to_string(),
        offset_seconds: Some(7_200),
    }
}

/// A stamp with no zone available, which is the only way `offset_seconds` is
/// null. Present so the inventory records that the field can be null at all.
fn zoneless() -> Stamp {
    Stamp {
        epoch: 0,
        local: "epoch 0".to_string(),
        zone: "unknown zone".to_string(),
        offset_seconds: None,
    }
}

fn report(
    path: &str,
    head: Head,
    tracking: Tracking,
    landed: Landed,
    unpushed: Unpushed,
) -> CheckoutReport {
    CheckoutReport {
        path: PathBuf::from(path),
        repo_key: RepoKey("/repos/app/.git".to_string()),
        repo_root: PathBuf::from("/repos/app"),
        is_linked_worktree: true,
        head,
        commits: Vec::new(),
        churn: Churn::default(),
        dirty: Dirty::default(),
        tracking,
        landed,
        unpushed,
        problems: vec!["something to say".to_string()],
    }
}

fn bare(report: CheckoutReport) -> CheckoutDigest {
    CheckoutDigest {
        report,
        workspaces: Vec::new(),
        agents: Vec::new(),
    }
}

/// Every enum arm and every optional field, in one document.
fn kitchen_sink() -> Digest {
    let commit = Commit {
        oid: "a".repeat(40),
        author: "Agent Smith".to_string(),
        committed: stamp(1_786_032_240),
        subject: "Do the thing".to_string(),
        is_merge: false,
        insertions: 10,
        deletions: 2,
        files: vec!["src/a.rs".to_string()],
    };

    let agent = AgentRef {
        name: Some("kestrel".to_string()),
        program: Some("claude".to_string()),
        session_id: Some("session-7f3a".to_string()),
        pane_id: "%1".to_string(),
        status: Some("working".to_string()),
        cwd: Some(PathBuf::from("/repos/app")),
    };
    // The same shape with everything herdr may omit left out, so the inventory
    // records that each of these can be null.
    let anonymous = AgentRef {
        name: None,
        program: None,
        session_id: None,
        pane_id: "%2".to_string(),
        status: None,
        cwd: None,
    };

    let mut occupied = report(
        "/repos/app",
        Head::Branch {
            name: "main".to_string(),
            oid: "b".repeat(40),
        },
        Tracking::Upstream {
            name: "origin/main".to_string(),
            ahead: 1,
            behind: 2,
        },
        Landed::IsDefault {
            name: "main".to_string(),
        },
        Unpushed::Commits { count: 3 },
    );
    occupied.commits = vec![commit];
    occupied.churn = Churn {
        files: 2,
        excluded: 1,
        insertions: 10,
        deletions: 2,
    };
    occupied.dirty = Dirty {
        tracked_changed: 1,
        untracked: 2,
        conflicted: 3,
        insertions: 4,
        deletions: 5,
    };

    let checkouts = vec![
        CheckoutDigest {
            report: occupied,
            workspaces: vec![
                WorkspaceRef {
                    workspace_id: "ws-1".to_string(),
                    label: "media".to_string(),
                    number: Some(3),
                    paths: vec![PathBuf::from("/repos/app")],
                    agents: vec![agent.clone()],
                    agent_status: Some("working".to_string()),
                },
                WorkspaceRef {
                    workspace_id: "ws-2".to_string(),
                    label: "spare".to_string(),
                    number: None,
                    paths: Vec::new(),
                    agents: Vec::new(),
                    agent_status: None,
                },
            ],
            agents: vec![agent, anonymous],
        },
        bare(report(
            "/repos/app/wt-detached",
            Head::Detached {
                oid: "c".repeat(40),
            },
            Tracking::NotApplicable,
            Landed::Merged {
                into: "origin/main".to_string(),
            },
            Unpushed::NoRemote,
        )),
        bare(report(
            "/repos/app/wt-unborn",
            Head::Unborn {
                name: "main".to_string(),
            },
            Tracking::NoUpstream,
            Landed::Equivalent {
                into: "origin/main".to_string(),
                how: Equivalence::EveryCommit { commits: 2 },
            },
            Unpushed::Unknown {
                reason: "could not be read".to_string(),
            },
        )),
        bare(report(
            "/repos/app/wt-deleted",
            Head::BranchDeleted {
                name: "wip/salvage".to_string(),
            },
            Tracking::UpstreamMissing {
                name: "origin/wip".to_string(),
            },
            Landed::Equivalent {
                into: "origin/main".to_string(),
                how: Equivalence::Squashed {
                    oid: "d".repeat(40),
                },
            },
            Unpushed::Commits { count: 0 },
        )),
        bare(report(
            "/repos/app/wt-topic",
            Head::Branch {
                name: "topic".to_string(),
                oid: "e".repeat(40),
            },
            Tracking::NoUpstream,
            Landed::NotMerged {
                into: "origin/main".to_string(),
            },
            Unpushed::Commits { count: 1 },
        )),
        bare(report(
            "/repos/app/wt-unknown",
            Head::Detached {
                oid: "f".repeat(40),
            },
            Tracking::NotApplicable,
            Landed::Unknown {
                reason: "no default branch".to_string(),
            },
            Unpushed::NoRemote,
        )),
    ];

    Digest {
        schema: SCHEMA_VERSION,
        generated_at: stamp(1_786_033_920),
        window: Window {
            since: stamp(1_786_003_200),
            until: Some(zoneless()),
            source: WindowSource::SinceLast {
                previous_run: stamp(1_785_916_800),
            },
        },
        repos: vec![RepoDigest {
            repo_key: RepoKey("/repos/app/.git".to_string()),
            name: "app".to_string(),
            repo_root: PathBuf::from("/repos/app"),
            checkouts,
            commits: 1,
            churn: Churn::default(),
            active_days: 1,
        }],
        notes: vec![Note::info("for information"), Note::warning("louder")],
    }
}

/// The window sources the kitchen sink cannot hold at once — there is one window
/// per digest, so the rest are rendered separately and unioned in.
fn other_windows() -> Vec<Digest> {
    [
        WindowSource::Default,
        WindowSource::Explicit {
            spec: "yesterday".to_string(),
        },
        WindowSource::SinceLastFirstRun,
        WindowSource::Rollup {
            period: Period::Week,
        },
        WindowSource::Rollup {
            period: Period::Month,
        },
    ]
    .into_iter()
    .map(|source| {
        let mut digest = kitchen_sink();
        digest.window.source = source;
        digest
    })
    .collect()
}

fn digest_inventory() -> BTreeSet<String> {
    let mut all = inventory(&render::json(&kitchen_sink()).expect("json"));
    for digest in other_windows() {
        all.extend(inventory(&render::json(&digest).expect("json")));
    }
    all
}

// ---------------------------------------------------------------------------
// The digest document
// ---------------------------------------------------------------------------

/// Every path `standup --json` can produce.
const DIGEST_PATHS: &[&str] = &[
    "generated_at: object",
    "generated_at.epoch: number",
    "generated_at.local: string",
    "generated_at.offset_seconds: number",
    "generated_at.zone: string",
    "notes: array",
    "notes[]: object",
    "notes[].message: string",
    "notes[].severity: string",
    "repos: array",
    "repos[]: object",
    "repos[].active_days: number",
    "repos[].checkouts: array",
    "repos[].checkouts[]: object",
    "repos[].checkouts[].agents: array",
    "repos[].checkouts[].agents[]: object",
    "repos[].checkouts[].agents[].cwd: null",
    "repos[].checkouts[].agents[].cwd: string",
    "repos[].checkouts[].agents[].name: null",
    "repos[].checkouts[].agents[].name: string",
    "repos[].checkouts[].agents[].pane_id: string",
    "repos[].checkouts[].agents[].program: null",
    "repos[].checkouts[].agents[].program: string",
    "repos[].checkouts[].agents[].session_id: null",
    "repos[].checkouts[].agents[].session_id: string",
    "repos[].checkouts[].agents[].status: null",
    "repos[].checkouts[].agents[].status: string",
    "repos[].checkouts[].churn: object",
    "repos[].checkouts[].churn.deletions: number",
    "repos[].checkouts[].churn.excluded: number",
    "repos[].checkouts[].churn.files: number",
    "repos[].checkouts[].churn.insertions: number",
    "repos[].checkouts[].commits: array",
    "repos[].checkouts[].commits[]: object",
    "repos[].checkouts[].commits[].author: string",
    "repos[].checkouts[].commits[].committed: object",
    "repos[].checkouts[].commits[].committed.epoch: number",
    "repos[].checkouts[].commits[].committed.local: string",
    "repos[].checkouts[].commits[].committed.offset_seconds: number",
    "repos[].checkouts[].commits[].committed.zone: string",
    "repos[].checkouts[].commits[].deletions: number",
    "repos[].checkouts[].commits[].files: array",
    "repos[].checkouts[].commits[].files[]: string",
    "repos[].checkouts[].commits[].insertions: number",
    "repos[].checkouts[].commits[].is_merge: bool",
    "repos[].checkouts[].commits[].oid: string",
    "repos[].checkouts[].commits[].subject: string",
    "repos[].checkouts[].dirty: object",
    "repos[].checkouts[].dirty.conflicted: number",
    "repos[].checkouts[].dirty.deletions: number",
    "repos[].checkouts[].dirty.insertions: number",
    "repos[].checkouts[].dirty.tracked_changed: number",
    "repos[].checkouts[].dirty.untracked: number",
    "repos[].checkouts[].head: object",
    "repos[].checkouts[].head.kind: string",
    "repos[].checkouts[].head.name: string",
    "repos[].checkouts[].head.oid: string",
    "repos[].checkouts[].is_linked_worktree: bool",
    "repos[].checkouts[].landed: object",
    "repos[].checkouts[].landed.how: object",
    "repos[].checkouts[].landed.how.commits: number",
    "repos[].checkouts[].landed.how.kind: string",
    "repos[].checkouts[].landed.how.oid: string",
    "repos[].checkouts[].landed.into: string",
    "repos[].checkouts[].landed.kind: string",
    "repos[].checkouts[].landed.name: string",
    "repos[].checkouts[].landed.reason: string",
    "repos[].checkouts[].path: string",
    "repos[].checkouts[].problems: array",
    "repos[].checkouts[].problems[]: string",
    "repos[].checkouts[].repo_key: string",
    "repos[].checkouts[].repo_root: string",
    "repos[].checkouts[].tracking: object",
    "repos[].checkouts[].tracking.ahead: number",
    "repos[].checkouts[].tracking.behind: number",
    "repos[].checkouts[].tracking.kind: string",
    "repos[].checkouts[].tracking.name: string",
    "repos[].checkouts[].unpushed: object",
    "repos[].checkouts[].unpushed.count: number",
    "repos[].checkouts[].unpushed.kind: string",
    "repos[].checkouts[].unpushed.reason: string",
    "repos[].checkouts[].workspaces: array",
    "repos[].checkouts[].workspaces[]: object",
    "repos[].checkouts[].workspaces[].agent_status: null",
    "repos[].checkouts[].workspaces[].agent_status: string",
    "repos[].checkouts[].workspaces[].agents: array",
    "repos[].checkouts[].workspaces[].agents[]: object",
    "repos[].checkouts[].workspaces[].agents[].cwd: string",
    "repos[].checkouts[].workspaces[].agents[].name: string",
    "repos[].checkouts[].workspaces[].agents[].pane_id: string",
    "repos[].checkouts[].workspaces[].agents[].program: string",
    "repos[].checkouts[].workspaces[].agents[].session_id: string",
    "repos[].checkouts[].workspaces[].agents[].status: string",
    "repos[].checkouts[].workspaces[].label: string",
    "repos[].checkouts[].workspaces[].number: null",
    "repos[].checkouts[].workspaces[].number: number",
    "repos[].checkouts[].workspaces[].paths: array",
    "repos[].checkouts[].workspaces[].paths[]: string",
    "repos[].checkouts[].workspaces[].workspace_id: string",
    "repos[].churn: object",
    "repos[].churn.deletions: number",
    "repos[].churn.excluded: number",
    "repos[].churn.files: number",
    "repos[].churn.insertions: number",
    "repos[].commits: number",
    "repos[].name: string",
    "repos[].repo_key: string",
    "repos[].repo_root: string",
    "schema: number",
    "window: object",
    "window.since: object",
    "window.since.epoch: number",
    "window.since.local: string",
    "window.since.offset_seconds: number",
    "window.since.zone: string",
    "window.source: object",
    "window.source.kind: string",
    "window.source.period: string",
    "window.source.previous_run: object",
    "window.source.previous_run.epoch: number",
    "window.source.previous_run.local: string",
    "window.source.previous_run.offset_seconds: number",
    "window.source.previous_run.zone: string",
    "window.source.spec: string",
    "window.until: object",
    "window.until.epoch: number",
    "window.until.local: string",
    "window.until.offset_seconds: null",
    "window.until.zone: string",
];

/// Every `kind` a digest can carry.
const DIGEST_KINDS: &[&str] = &[
    "branch",
    "branch_deleted",
    "commits",
    "default",
    "detached",
    "equivalent",
    "every_commit",
    "explicit",
    "is_default",
    "merged",
    "no_remote",
    "no_upstream",
    "not_applicable",
    "not_merged",
    "rollup",
    "since_last",
    "since_last_first_run",
    "squashed",
    "unborn",
    "unknown",
    "upstream",
    "upstream_missing",
];

#[test]
fn the_digest_shape_is_the_documented_one() {
    compare_sets(&digest_inventory(), DIGEST_PATHS, "the digest's paths");
}

#[test]
fn the_digest_kinds_are_the_documented_ones() {
    let mut all = all_kinds(&render::json(&kitchen_sink()).expect("json"));
    for digest in other_windows() {
        all.extend(all_kinds(&render::json(&digest).expect("json")));
    }
    compare_sets(&all, DIGEST_KINDS, "the digest's kinds");
}

// ---------------------------------------------------------------------------
// The comparison document
// ---------------------------------------------------------------------------

/// Every path `standup --diff --json` can produce.
const COMPARISON_PATHS: &[&str] = &[
    "after: object",
    "after.epoch: number",
    "after.local: string",
    "after.offset_seconds: number",
    "after.zone: string",
    "before: object",
    "before.epoch: number",
    "before.local: string",
    "before.offset_seconds: number",
    "before.zone: string",
    "repos: array",
    "repos[]: object",
    "repos[].checkouts: array",
    "repos[].checkouts[]: array",
    "repos[].checkouts[][]: object",
    "repos[].checkouts[][]: string",
    "repos[].checkouts[][].commits: number",
    "repos[].checkouts[][].kind: string",
    "repos[].checkouts[][].landed: bool",
    "repos[].checkouts[][].unpushed: number",
    "repos[].checkouts[][].uncommitted: bool",
    "repos[].checkouts[][].was_holding: number",
    "repos[].commits: number",
    "repos[].name: string",
    "repos[].repo_key: string",
    "schema: number",
];

const COMPARISON_KINDS: &[&str] = &[
    "advanced",
    "appeared",
    "gone",
    "landed",
    "pushed",
    "stalled",
    "unchanged",
];

/// A pair of digests that produces every [`standup::model::Movement`].
fn moved() -> (Digest, Digest) {
    // Built by moving one repository's checkouts between two digests rather than
    // by constructing `Movement` values, so the inventory covers what `compare`
    // actually emits.
    let mut before = kitchen_sink();
    let mut after = kitchen_sink();

    // Advanced and landed: the topic checkout gains a commit and reaches the
    // trunk.
    let commit = Commit {
        oid: "9".repeat(40),
        author: "Agent Smith".to_string(),
        committed: stamp(1_786_032_240),
        subject: "New work".to_string(),
        is_merge: false,
        insertions: 1,
        deletions: 0,
        files: vec!["src/b.rs".to_string()],
    };
    let topic = 4;
    after.repos[0].checkouts[topic].report.commits = vec![commit];
    after.repos[0].checkouts[topic].report.landed = Landed::Merged {
        into: "origin/main".to_string(),
    };

    // Pushed: the at-risk count falls to nothing.
    before.repos[0].checkouts[1].report.unpushed = Unpushed::Commits { count: 2 };
    after.repos[0].checkouts[1].report.unpushed = Unpushed::Commits { count: 0 };

    // Stalled: still holding work, no new commits.
    after.repos[0].checkouts[3].report.dirty = Dirty {
        tracked_changed: 1,
        ..Dirty::default()
    };

    // Landed without new commits.
    before.repos[0].checkouts[5].report.landed = Landed::NotMerged {
        into: "origin/main".to_string(),
    };
    after.repos[0].checkouts[5].report.landed = Landed::IsDefault {
        name: "main".to_string(),
    };

    // Gone, holding unpushed commits: present before, absent after.
    let mut vanished = bare(report(
        "/repos/app/wt-vanished",
        Head::Branch {
            name: "gone".to_string(),
            oid: "8".repeat(40),
        },
        Tracking::NoUpstream,
        Landed::NotMerged {
            into: "origin/main".to_string(),
        },
        Unpushed::Commits { count: 4 },
    ));
    vanished.report.problems.clear();
    before.repos[0].checkouts.push(vanished);

    // Appeared: absent before, present after, with a commit.
    let mut fresh = bare(report(
        "/repos/app/wt-fresh",
        Head::Branch {
            name: "fresh".to_string(),
            oid: "7".repeat(40),
        },
        Tracking::NoUpstream,
        Landed::NotMerged {
            into: "origin/main".to_string(),
        },
        Unpushed::Commits { count: 1 },
    ));
    fresh.report.commits = vec![Commit {
        oid: "6".repeat(40),
        author: "Agent Smith".to_string(),
        committed: stamp(1_786_032_240),
        subject: "Brand new".to_string(),
        is_merge: false,
        insertions: 1,
        deletions: 0,
        files: vec!["src/c.rs".to_string()],
    }];
    after.repos[0].checkouts.push(fresh);

    (before, after)
}

#[test]
fn the_comparison_shape_is_the_documented_one() {
    let (before, after) = moved();
    let comparison = compare::compare(&before, &after);
    let json = serde_json::to_string(&comparison).expect("json");
    compare_sets(
        &inventory(&json),
        COMPARISON_PATHS,
        "the comparison's paths",
    );
}

#[test]
fn the_comparison_kinds_are_the_documented_ones() {
    let (before, after) = moved();
    let comparison = compare::compare(&before, &after);
    let json = serde_json::to_string(&comparison).expect("json");
    compare_sets(
        &all_kinds(&json),
        COMPARISON_KINDS,
        "the comparison's kinds",
    );
}

#[test]
fn the_comparison_carries_the_same_schema_as_the_digest() {
    // Both documents are for scripting, and a consumer of `--diff --json` needs
    // the same right to refuse a shape it does not know.
    let (before, after) = moved();
    let comparison = compare::compare(&before, &after);
    assert_eq!(comparison.schema, SCHEMA_VERSION);
    let json = serde_json::to_string(&comparison).expect("json");
    assert!(
        json.contains(&format!("\"schema\":{SCHEMA_VERSION}")),
        "the version must reach the document:\n{json}"
    );
}

// ---------------------------------------------------------------------------
// The version and its paperwork
// ---------------------------------------------------------------------------

#[test]
fn the_documented_version_is_the_emitted_one() {
    // The contract is `docs/json-schema.md`. A version bumped in code and not
    // there is a promise nobody can read.
    let doc = std::fs::read_to_string("docs/json-schema.md").expect("the schema contract exists");
    let line = format!("Current version: **{SCHEMA_VERSION}**");
    assert!(
        doc.contains(&line),
        "docs/json-schema.md must say `{line}`.\n\n{RULE}"
    );
    // And the history table has to have a row for it.
    let row = format!("| {SCHEMA_VERSION} |");
    assert!(
        doc.contains(&row),
        "docs/json-schema.md needs a history row starting `{row}`.\n\n{RULE}"
    );
}

#[test]
fn a_bumped_version_has_to_be_announced() {
    // Dormant at version 1, which was never published, and load-bearing the
    // moment somebody bumps: the changelog is where a consumer finds out.
    if SCHEMA_VERSION == 1 {
        return;
    }
    let changelog = std::fs::read_to_string("CHANGELOG.md").expect("a changelog");
    let needle = format!("schema {SCHEMA_VERSION}");
    assert!(
        changelog.contains(&needle),
        "CHANGELOG.md must mention `{needle}`.\n\n{RULE}"
    );
}
