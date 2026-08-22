//! Comparing two digests.
//!
//! `--since-last` answers "what happened since I last looked". This answers the
//! question after it: **what changed between two digests** — which is not the
//! same report as one longer window, and must not read like one.
//!
//! A longer window tells you more happened. A comparison tells you what *moved*:
//! work that is new, work that finished, and work that stalled. The third is the
//! one worth having, because a checkout that was busy yesterday and is holding
//! uncommitted work today is the thing a digest of either day states plainly and
//! neither day flags.
//!
//! # What is compared, and what is not
//!
//! Checkouts are matched **by path**, which is the only identity a checkout has
//! across two runs: a branch gets renamed, a worktree gets recreated at the same
//! place, `HEAD` moves constantly. Commits are matched by oid, so a rebase
//! between the two runs reads as new work — which it is, in the sense that
//! matters here: those objects were not in the earlier digest.
//!
//! Nothing here runs git. A comparison is a pure function of two digests, so it
//! says exactly what those two digests said and cannot quietly consult the disk
//! for a third answer.

use crate::model::{
    Activity, CheckoutDigest, Comparison, Digest, Landed, Movement, RepoComparison,
};

/// Compares an earlier digest with a later one.
pub fn compare(before: &Digest, after: &Digest) -> Comparison {
    let mut repos: Vec<RepoComparison> = Vec::new();

    for repo in &after.repos {
        let previous = before
            .repos
            .iter()
            .find(|old| old.repo_key == repo.repo_key);
        let mut checkouts: Vec<(String, Movement)> = Vec::new();

        for checkout in &repo.checkouts {
            let path = checkout.report.path.display().to_string();
            let was = previous.and_then(|old| {
                old.checkouts
                    .iter()
                    .find(|old| old.report.path == checkout.report.path)
            });
            checkouts.push((path, movement(was, checkout)));
        }

        // A checkout in the earlier digest and not the later one is gone: the
        // worktree was removed, or the workspace that found it was closed. Worth
        // saying, because it is where unpushed work goes to die.
        if let Some(previous) = previous {
            for old in &previous.checkouts {
                if repo
                    .checkouts
                    .iter()
                    .any(|now| now.report.path == old.report.path)
                {
                    continue;
                }
                checkouts.push((
                    old.report.path.display().to_string(),
                    Movement::Gone {
                        was_holding: old.report.unpushed.at_risk(),
                    },
                ));
            }
        }

        repos.push(RepoComparison {
            repo_key: repo.repo_key.clone(),
            name: repo.name.clone(),
            commits: repo
                .commits
                .saturating_sub(previous.map_or(0, |old| old.commits)),
            checkouts,
        });
    }

    // Repositories that were in the earlier digest and are not in the later one.
    // Not the same as a removed checkout: the whole repository is out of the
    // session, which usually means every workspace on it was closed.
    for old in &before.repos {
        if after.repos.iter().any(|now| now.repo_key == old.repo_key) {
            continue;
        }
        repos.push(RepoComparison {
            repo_key: old.repo_key.clone(),
            name: old.name.clone(),
            commits: 0,
            checkouts: old
                .checkouts
                .iter()
                .map(|checkout| {
                    (
                        checkout.report.path.display().to_string(),
                        Movement::Gone {
                            was_holding: checkout.report.unpushed.at_risk(),
                        },
                    )
                })
                .collect(),
        });
    }

    repos.sort_by(|a, b| {
        rank(b)
            .cmp(&rank(a))
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.name.cmp(&b.name))
    });

    Comparison {
        schema: crate::model::SCHEMA_VERSION,
        before: before.generated_at.clone(),
        after: after.generated_at.clone(),
        repos,
    }
}

/// What happened to one checkout between the two digests.
///
/// The order of these tests is the answer to "what does a reader most need to
/// know". New work first, because it is what the digest is for. Then work that
/// finished, because it is what can be forgotten about. Then stalled, because it
/// is the one neither digest flags on its own.
fn movement(was: Option<&CheckoutDigest>, now: &CheckoutDigest) -> Movement {
    let Some(was) = was else {
        return Movement::Appeared {
            commits: now.report.commits.len(),
        };
    };

    let new_commits = now
        .report
        .commits
        .iter()
        .filter(|commit| !was.report.commits.iter().any(|old| old.oid == commit.oid))
        .count();
    if new_commits > 0 {
        return Movement::Advanced {
            commits: new_commits,
            landed: landed_since(was, now),
        };
    }

    if landed_since(was, now) {
        return Movement::Landed;
    }

    // Pushed since: the at-risk count fell to nothing. Distinct from landing,
    // which is about the trunk, and the reason both are reported.
    if was.report.unpushed.at_risk() > 0 && now.report.unpushed.at_risk() == 0 {
        return Movement::Pushed {
            was_holding: was.report.unpushed.at_risk(),
        };
    }

    // No new commits, and still holding something that would be lost. This is
    // the comparison's own finding: each digest on its own reports the state
    // plainly and neither says it has not moved.
    let holding = now.report.unpushed.at_risk();
    let uncommitted = !now.report.dirty.is_clean();
    if holding > 0 || uncommitted {
        return Movement::Stalled {
            unpushed: holding,
            uncommitted,
        };
    }

    Movement::Unchanged
}

/// Whether the work reached the trunk between the two digests.
///
/// Both readings of "in" count — containment and a matching patch — because a
/// squash merge is how most of it arrives, and reporting that as "not landed
/// yet" is the bug #7 fixed. What is required is that it was *not* in before and
/// is now.
fn landed_since(was: &CheckoutDigest, now: &CheckoutDigest) -> bool {
    !is_in(&was.report.landed) && is_in(&now.report.landed)
}

fn is_in(landed: &Landed) -> bool {
    matches!(
        landed,
        Landed::Merged { .. } | Landed::Equivalent { .. } | Landed::IsDefault { .. }
    )
}

/// Sort key, in the order the request names: what is new, what finished, what
/// stalled. Work that *vanished while holding unpushed commits* goes above all
/// of them, because that is the only case here where something was lost rather
/// than merely not progressed.
///
/// New work above stalled deliberately. Stalled is a nudge and is marked as one;
/// putting it first would bury the commits a reader opened the comparison to
/// see, which is the same mistake as burying a busy repository under quiet ones.
fn rank(repo: &RepoComparison) -> u8 {
    repo.checkouts
        .iter()
        .map(|(_, movement)| match movement {
            Movement::Gone { was_holding } if *was_holding > 0 => 5,
            Movement::Advanced { .. } | Movement::Appeared { .. } => 4,
            Movement::Landed | Movement::Pushed { .. } => 3,
            Movement::Stalled { .. } => 2,
            Movement::Gone { .. } => 1,
            Movement::Unchanged => 0,
        })
        .max()
        .unwrap_or(0)
}

impl Comparison {
    /// Whether anything moved at all. An empty comparison is a real answer and
    /// has to read as one rather than as an empty screen.
    pub fn is_quiet(&self) -> bool {
        self.repos.iter().all(|repo| {
            repo.checkouts
                .iter()
                .all(|(_, movement)| matches!(movement, Movement::Unchanged))
        })
    }

    /// Total new commits across every repository.
    pub fn total_commits(&self) -> usize {
        self.repos.iter().map(|repo| repo.commits).sum()
    }
}

impl Movement {
    /// The words a reader scans for. Each one names what moved, never how much
    /// happened in total — that is what a digest is for.
    pub fn sentence(&self) -> String {
        match self {
            Movement::Appeared { commits } => match commits {
                0 => "new here".to_string(),
                many => format!("new here, with {many} in the window"),
            },
            Movement::Advanced { commits, landed } => {
                let counted = format!("{commits} new since");
                if *landed {
                    format!("{counted}, and landed")
                } else {
                    counted
                }
            }
            Movement::Landed => "landed since".to_string(),
            Movement::Pushed { was_holding } => {
                format!("pushed since; {was_holding} no longer only here")
            }
            Movement::Stalled {
                unpushed,
                uncommitted,
            } => {
                let mut held = Vec::new();
                if *unpushed > 0 {
                    held.push(format!("{unpushed} unpushed"));
                }
                if *uncommitted {
                    held.push("uncommitted work".to_string());
                }
                format!("no new commits, still holding {}", held.join(" and "))
            }
            Movement::Gone { was_holding } => match was_holding {
                0 => "gone".to_string(),
                many => format!("gone, and was holding {many} that were only there"),
            },
            Movement::Unchanged => "unchanged".to_string(),
        }
    }

    /// Whether a reader has to act on this. Used by both renderers so the
    /// marking is identical.
    pub fn loud(&self) -> bool {
        matches!(
            self,
            Movement::Stalled { .. } | Movement::Gone { was_holding: 1.. }
        )
    }

    /// Mirrors `Activity`: a comparison with nothing to say is summarised rather
    /// than listed line by line.
    pub fn activity(&self) -> Activity {
        match self {
            Movement::Unchanged => Activity::Quiet,
            _ => Activity::Active,
        }
    }
}

/// Reads a digest that an earlier `--json` run wrote.
///
/// The schema version is checked before anything else. The JSON is documented as
/// being for scripting, and a shape this binary does not know is refused by name
/// rather than deserialised into something that happens to fit — a comparison
/// built on a misread digest would be confidently wrong about what moved.
pub fn read_digest(path: &std::path::Path) -> crate::Result<Digest> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read the digest at {}: {err}", path.display()))?;

    // Peeked before deserialising, so a version mismatch is reported as a
    // version mismatch rather than as whichever field happened to move.
    let peek: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{} is not JSON standup wrote: {err}", path.display()))?;
    match peek.get("schema").and_then(serde_json::Value::as_u64) {
        Some(schema) if schema == u64::from(crate::model::SCHEMA_VERSION) => {}
        Some(schema) => {
            return Err(format!(
                "{} was written with schema {schema}, and this standup reads {}. \
                 Regenerate it with `standup --json`.",
                path.display(),
                crate::model::SCHEMA_VERSION
            )
            .into())
        }
        None => {
            return Err(format!(
                "{} has no `schema` field, so it is not a digest standup wrote. \
                 `standup --json > that-file` produces one.",
                path.display()
            )
            .into())
        }
    }

    serde_json::from_str(&raw)
        .map_err(|err| format!("could not read the digest at {}: {err}", path.display()).into())
}
