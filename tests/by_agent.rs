//! Grouping by agent.
//!
//! Two claims are load-bearing here, and the tests are shaped around them.
//!
//! 1. **It is never the default.** Repository grouping keeps a branch's commits
//!    together; agent grouping interleaves unrelated projects. Every assertion
//!    about the default form is written against the rendered output, not against
//!    the config field, because the field being `false` proves nothing about what
//!    a reader sees.
//! 2. **The totals stop reconciling, and the digest says so with a number.** A
//!    commit reaches two groups by two routes and only one of them is obvious, so
//!    the discrepancy is measured against the digest's own total rather than
//!    inferred from the shapes that produce it.

use std::path::PathBuf;

use standup::by_agent::{self, UNATTRIBUTED};
use standup::config::{Config, Format, Ignored};
use standup::model::{
    AgentRef, CheckoutDigest, CheckoutReport, Churn, Commit, Digest, Dirty, Head, Landed,
    RepoDigest, RepoKey, Stamp, Tracking, Unpushed, Window, WindowSource, SCHEMA_VERSION,
};
use standup::render;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn stamp(local: &str, epoch: i64) -> Stamp {
    Stamp {
        epoch,
        local: local.to_string(),
        zone: "CEST +0200".to_string(),
        offset_seconds: Some(7_200),
    }
}

fn commit(oid: &str, epoch: i64, subject: &str, files: &[&str]) -> Commit {
    Commit {
        oid: oid.to_string(),
        author: "Agent Smith".to_string(),
        committed: stamp("2026-08-15 09:04", epoch),
        subject: subject.to_string(),
        files: files.iter().map(|f| f.to_string()).collect(),
        insertions: 10,
        deletions: 4,
        is_merge: false,
    }
}

/// A checkout of `repo` at `path`, carrying `commits`.
fn checkout(repo: &str, path: &str, commits: Vec<Commit>) -> CheckoutReport {
    let mut churn = Churn::default();
    let mut files: Vec<&str> = Vec::new();
    for commit in &commits {
        churn.insertions += commit.insertions;
        churn.deletions += commit.deletions;
        for file in &commit.files {
            if !files.contains(&file.as_str()) {
                files.push(file);
            }
        }
    }
    churn.files = files.len();
    CheckoutReport {
        path: PathBuf::from(path),
        repo_key: RepoKey(format!("/repos/{repo}/.git")),
        repo_root: PathBuf::from(format!("/repos/{repo}")),
        is_linked_worktree: false,
        head: Head::Branch {
            name: "main".to_string(),
            oid: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_string(),
        },
        commits,
        churn,
        dirty: Dirty::default(),
        tracking: Tracking::NoUpstream,
        landed: Landed::IsDefault {
            name: "main".to_string(),
        },
        unpushed: Unpushed::Commits { count: 0 },
        problems: Vec::new(),
    }
}

fn agent(pane: &str, name: &str) -> AgentRef {
    AgentRef {
        name: Some(name.to_string()),
        program: Some("claude".to_string()),
        session_id: Some("session-7f3a".to_string()),
        pane_id: pane.to_string(),
        status: Some("working".to_string()),
        cwd: Some(PathBuf::from("/repos/app")),
    }
}

fn placed(report: CheckoutReport, agents: Vec<AgentRef>) -> CheckoutDigest {
    CheckoutDigest {
        report,
        workspaces: Vec::new(),
        agents,
    }
}

/// The same rollup rule `standup::rollup` applies: distinct commits, the union of
/// touched paths, and the excluded count recomputed over that union.
fn repo(name: &str, checkouts: Vec<CheckoutDigest>) -> RepoDigest {
    let mut seen: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut churn = Churn::default();
    for checkout in &checkouts {
        for commit in &checkout.report.commits {
            if seen.contains(&commit.oid) {
                continue;
            }
            seen.push(commit.oid.clone());
            churn.insertions += commit.insertions;
            churn.deletions += commit.deletions;
            for file in &commit.files {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
        }
    }
    churn.files = files.len();
    churn.excluded = files
        .iter()
        .filter(|file| Ignored::default().matches(file))
        .count();
    RepoDigest {
        repo_key: RepoKey(format!("/repos/{name}/.git")),
        name: name.to_string(),
        repo_root: PathBuf::from(format!("/repos/{name}")),
        checkouts,
        commits: seen.len(),
        churn,
        active_days: 1,
    }
}

fn digest(repos: Vec<RepoDigest>) -> Digest {
    Digest {
        schema: SCHEMA_VERSION,
        generated_at: stamp("2026-08-15 09:12", 1_786_033_920),
        window: Window {
            since: stamp("2026-08-15 00:00", 1_786_003_200),
            until: None,
            source: WindowSource::Default,
        },
        repos,
        notes: Vec::new(),
    }
}

/// Two agents, each in their own checkout of their own repository. The ordinary
/// shape: nothing is shared, and the totals reconcile.
fn two_agents_two_repos() -> Digest {
    digest(vec![
        repo(
            "app",
            vec![placed(
                checkout(
                    "app",
                    "/repos/app",
                    vec![commit(
                        "aaaa0001",
                        1_786_030_000,
                        "Wire the fetcher",
                        &["src/a.rs"],
                    )],
                ),
                vec![agent("%1", "kestrel")],
            )],
        ),
        repo(
            "docs",
            vec![placed(
                checkout(
                    "docs",
                    "/repos/docs",
                    vec![commit(
                        "bbbb0001",
                        1_786_031_000,
                        "Write the guide",
                        &["docs/x.md"],
                    )],
                ),
                vec![agent("%2", "wren")],
            )],
        ),
    ])
}

fn group(digest: &Digest) -> by_agent::Grouping {
    by_agent::group(digest, &Ignored::default())
}

fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The same text with each format's emphasis removed, so one assertion can name
/// a heading in all four. Written to strip markup only: a missing heading cannot
/// pass because a `*` was eaten.
fn bare(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '*' | '`' | '\u{2014}' => out.push(' '),
            _ => out.push(ch),
        }
    }
    flatten(&out)
}

fn grouped(config_format: Format, digest: &Digest) -> String {
    let config = Config {
        format: config_format,
        by_agent: true,
        ..Config::default()
    };
    render::render(digest, &config).expect("render")
}

// ---------------------------------------------------------------------------
// Opt-in
// ---------------------------------------------------------------------------

#[test]
fn grouping_is_never_the_default() {
    assert!(
        !Config::default().by_agent,
        "repository grouping is the deliberate default"
    );
    assert!(
        !standup::config::load_with_args(&[])
            .expect("parse")
            .by_agent,
        "an empty command line must not group by agent"
    );
}

#[test]
fn the_flag_is_what_turns_it_on() {
    let parsed = standup::config::load_with_args(&["--by-agent".to_string()]).expect("parse");
    assert!(parsed.by_agent);
}

#[test]
fn the_default_rendering_names_no_agent_group() {
    let digest = two_agents_two_repos();
    let plain = flatten(&render::render(&digest, &Config::default()).expect("render"));
    assert!(
        !plain.contains("grouped by agent"),
        "the default form must not carry the grouping caveat:\n{plain}"
    );
    // The repository names are the headings in the default form, and the agent
    // names appear only where the ungrouped digest already credited them.
    assert!(plain.contains("app"), "{plain}");
    assert!(plain.contains("docs"), "{plain}");
}

// ---------------------------------------------------------------------------
// What a group holds
// ---------------------------------------------------------------------------

#[test]
fn an_agent_group_holds_only_the_checkouts_that_agent_occupied() {
    let grouping = group(&two_agents_two_repos());
    let labels: Vec<&str> = grouping
        .groups
        .iter()
        .map(|group| group.label.as_str())
        .collect();
    // The bare display name: the program suffix is the renderers' shared agent
    // label, so the grouping key stays whatever the ungrouped digest credited.
    assert_eq!(labels, vec!["kestrel", "wren"]);

    let kestrel = &grouping.groups[0];
    assert_eq!(kestrel.repos.len(), 1, "kestrel touched one repository");
    assert_eq!(kestrel.repos[0].name, "app");
    assert_eq!(kestrel.commits, 1);
}

#[test]
fn work_no_agent_reported_is_named_rather_than_dropped() {
    let digest = digest(vec![repo(
        "app",
        vec![placed(
            checkout(
                "app",
                "/repos/app",
                vec![commit("aaaa0001", 1_786_030_000, "Land it", &["src/a.rs"])],
            ),
            Vec::new(),
        )],
    )]);
    let grouping = group(&digest);
    assert_eq!(grouping.groups.len(), 1);
    assert_eq!(grouping.groups[0].label, UNATTRIBUTED);
    assert_eq!(grouping.groups[0].commits, 1, "the work happened");
    assert!(
        grouping.groups[0].agent.is_none(),
        "no agent may be invented for it"
    );
    // A sentence, not a name: it must not read as an agent called "unattributed".
    assert!(UNATTRIBUTED.contains(' '), "{UNATTRIBUTED}");
}

#[test]
fn a_group_total_is_recomputed_over_its_own_checkouts() {
    // One repository, two checkouts, one commit each, a different agent in each.
    // The repository's own total is 2; neither group may report 2.
    let digest = digest(vec![repo(
        "app",
        vec![
            placed(
                checkout(
                    "app",
                    "/repos/app/wt-1",
                    vec![commit("aaaa0001", 1_786_030_000, "One", &["src/a.rs"])],
                ),
                vec![agent("%1", "kestrel")],
            ),
            placed(
                checkout(
                    "app",
                    "/repos/app/wt-2",
                    vec![commit("bbbb0001", 1_786_031_000, "Two", &["src/b.rs"])],
                ),
                vec![agent("%2", "wren")],
            ),
        ],
    )]);
    assert_eq!(digest.repos[0].commits, 2, "the fixture is the shape meant");

    let grouping = group(&digest);
    assert_eq!(grouping.groups.len(), 2);
    for group in &grouping.groups {
        assert_eq!(
            group.commits, 1,
            "{} must not inherit the whole repository's count",
            group.label
        );
        assert_eq!(group.repos[0].checkouts.len(), 1);
    }
    assert_eq!(
        grouping.double_counted, 0,
        "distinct commits in distinct checkouts reconcile"
    );
}

// ---------------------------------------------------------------------------
// The numbers that stop reconciling
// ---------------------------------------------------------------------------

#[test]
fn a_checkout_two_agents_share_is_counted_under_each_and_says_so() {
    let digest = digest(vec![repo(
        "app",
        vec![placed(
            checkout(
                "app",
                "/repos/app",
                vec![
                    commit("aaaa0001", 1_786_030_000, "One", &["src/a.rs"]),
                    commit("bbbb0001", 1_786_031_000, "Two", &["src/b.rs"]),
                ],
            ),
            vec![agent("%1", "kestrel"), agent("%2", "wren")],
        )],
    )]);
    let grouping = group(&digest);

    assert_eq!(grouping.groups.len(), 2, "neither agent may be dropped");
    for group in &grouping.groups {
        assert_eq!(
            group.commits, 2,
            "{} gets both: nothing says which of them wrote which",
            group.label
        );
    }
    assert_eq!(grouping.shared, 1);
    assert_eq!(
        grouping.double_counted, 2,
        "4 grouped against 2 in the digest"
    );

    let caveat = grouping.caveat();
    assert!(
        caveat.contains("2 commits more"),
        "the caveat must carry the measured difference: {caveat}"
    );
}

#[test]
fn two_checkouts_of_one_repository_in_different_groups_are_measured() {
    // The route that is not obvious, and the one a live run actually produced:
    // one agent per checkout, nothing shared, and the same commit visible from
    // both worktrees because worktrees share history.
    let shared_commit = commit("aaaa0001", 1_786_030_000, "Shared history", &["src/a.rs"]);
    let digest = digest(vec![repo(
        "app",
        vec![
            placed(
                checkout("app", "/repos/app/wt-1", vec![shared_commit.clone()]),
                vec![agent("%1", "kestrel")],
            ),
            placed(
                checkout("app", "/repos/app/wt-2", vec![shared_commit]),
                vec![agent("%2", "wren")],
            ),
        ],
    )]);
    assert_eq!(
        digest.repos[0].commits, 1,
        "the digest counts the commit once, which is the whole point"
    );

    let grouping = group(&digest);
    assert_eq!(grouping.shared, 0, "no checkout has two agents in it");
    assert_eq!(
        grouping.double_counted, 1,
        "and yet the totals are one commit over"
    );

    let caveat = grouping.caveat();
    assert!(
        caveat.contains("1 commit more"),
        "singular, and measured: {caveat}"
    );
    assert!(
        caveat.contains("two checkouts of one repository"),
        "the reason a reader cannot guess must be named: {caveat}"
    );
}

#[test]
fn the_caveat_always_names_the_interleaving_hazard() {
    // Even when every number reconciles, the reading-order hazard is the reason
    // this is opt-in, so it is stated unconditionally.
    let grouping = group(&two_agents_two_repos());
    assert_eq!(grouping.double_counted, 0);
    let caveat = grouping.caveat();
    assert!(
        caveat.contains("interleaves unrelated projects"),
        "{caveat}"
    );
    assert!(
        caveat.contains("default grouping is by repository"),
        "and which grouping the reader gets without asking: {caveat}"
    );
}

// ---------------------------------------------------------------------------
// Every format
// ---------------------------------------------------------------------------

#[test]
fn every_format_carries_the_caveat_and_the_group_headings() {
    let digest = two_agents_two_repos();
    for format in [Format::Text, Format::Markdown, Format::Slack, Format::Html] {
        let out = flatten(&grouped(format, &digest));
        assert!(
            out.contains("grouped by agent"),
            "{format:?} dropped the caveat:\n{out}"
        );
        // The heading, not the per-checkout `agents:` line, which names the same
        // agent and would let a missing heading pass unnoticed. Each agent did
        // one commit; the heading is the only place that number appears.
        let bare = bare(&out);
        for agent in ["kestrel (claude)", "wren (claude)"] {
            assert!(
                bare.contains(&format!("{agent} 1 commit")),
                "{format:?} dropped the heading for {agent}, or its own count:\n{out}"
            );
        }
    }
}

#[test]
fn a_heading_names_the_agent_the_way_the_rest_of_the_digest_does() {
    // The grouping key is the bare display name; the digest credits an agent
    // with its program. A heading reading `kestrel` above a checkout line
    // reading `agents: kestrel (claude)` would be one agent named two ways.
    let grouping = group(&two_agents_two_repos());
    assert_eq!(grouping.groups[0].label, "kestrel");
    let out = flatten(&grouped(Format::Text, &two_agents_two_repos()));
    assert!(out.contains("agents: kestrel (claude)"), "{out}");
    assert!(
        out.contains("kestrel (claude) 1 commit"),
        "the heading must use the same credit:\n{out}"
    );
}

#[test]
fn a_quiet_repository_is_named_under_its_own_group() {
    // One repository, two checkouts: busy under kestrel, quiet under wren. A
    // single trailing "Quiet: app" would contradict the busy `app` block.
    let digest = digest(vec![repo(
        "app",
        vec![
            placed(
                checkout(
                    "app",
                    "/repos/app/wt-1",
                    vec![commit("aaaa0001", 1_786_030_000, "One", &["src/a.rs"])],
                ),
                vec![agent("%1", "kestrel")],
            ),
            placed(
                checkout("app", "/repos/app/wt-2", Vec::new()),
                vec![agent("%2", "wren")],
            ),
        ],
    )]);

    for format in [Format::Text, Format::Markdown, Format::Slack, Format::Html] {
        let out = flatten(&grouped(format, &digest));
        let quiet_at = out
            .to_lowercase()
            .find("quiet: app")
            .unwrap_or_else(|| panic!("{format:?} dropped the quiet repository:\n{out}"));
        let wren_at = out
            .find("wren (claude)")
            .unwrap_or_else(|| panic!("{format:?} dropped the wren heading:\n{out}"));
        let kestrel_at = out.find("kestrel (claude)").expect("kestrel heading");
        assert!(
            wren_at < quiet_at,
            "{format:?} must name the quiet repository under wren, not adrift at the end:\n{out}"
        );
        assert!(
            kestrel_at < wren_at,
            "{format:?} ordered the groups oddly, which this assertion depends on:\n{out}"
        );
    }
}

#[test]
fn the_ungrouped_digest_keeps_one_trailing_quiet_list() {
    // The scoping above must not leak into the default form, where a repository
    // is quiet or busy full stop and one list at the end is right.
    let digest = digest(vec![
        repo(
            "app",
            vec![placed(
                checkout(
                    "app",
                    "/repos/app",
                    vec![commit("aaaa0001", 1_786_030_000, "One", &["src/a.rs"])],
                ),
                vec![agent("%1", "kestrel")],
            )],
        ),
        repo(
            "docs",
            vec![placed(
                checkout("docs", "/repos/docs", Vec::new()),
                Vec::new(),
            )],
        ),
    ]);
    let out = flatten(&render::render(&digest, &Config::default()).expect("render"));
    assert!(out.to_lowercase().contains("quiet: docs"), "{out}");
    assert_eq!(
        out.to_lowercase().matches("quiet: ").count(),
        1,
        "exactly one list, at the end:\n{out}"
    );
}

#[test]
fn json_is_the_digest_and_is_not_regrouped() {
    // The JSON is a documented shape for scripting, versioned by SCHEMA_VERSION.
    // A view of it that reorganised the repositories under a flag would be a
    // second shape wearing the same version.
    let digest = two_agents_two_repos();
    let config = Config {
        format: Format::Json,
        by_agent: true,
        ..Config::default()
    };
    let out = render::render(&digest, &config).expect("render");
    assert!(
        !out.contains("grouped by agent"),
        "the machine-readable form must not gain prose:\n{out}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        parsed["repos"].as_array().expect("repos is an array").len(),
        2,
        "still grouped by repository"
    );
}

#[test]
fn the_group_heading_carries_that_groups_own_numbers() {
    let digest = two_agents_two_repos();
    let out = flatten(&grouped(Format::Text, &digest));
    // Each agent did one commit; the digest did two. The heading must show the
    // group's number, not the digest's.
    assert!(
        out.contains("kestrel (claude) 1 commit"),
        "the heading must carry the group's own count:\n{out}"
    );
}
