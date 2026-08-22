//! What the JSON says about time.
//!
//! A digest renders instants in local time, which is what a person wants and is
//! ambiguous for a script: `2026-08-15 09:12` is six different instants
//! depending on where it was written. So every stamp carries `offset_seconds`
//! beside it.
//!
//! The load-bearing assertion here is not that the field exists — it is that the
//! field **explains the string next to it**. `epoch` plus `offset_seconds` is
//! reduced to a civil date and time by arithmetic written from scratch below, and
//! compared with `local` character for character. An offset that was merely
//! present, or was the machine's current offset rather than the one that instant
//! was rendered in, would fail that.
//!
//! Everything runs the real binary under an explicit `TZ`, because the thing
//! being tested is what a consumer receives.

#[path = "fixtures.rs"]
mod fixtures;

use std::process::Command;

use fixtures::{Fixture, T_SINCE};

const BIN: &str = env!("CARGO_BIN_EXE_standup");

/// Runs a digest over one checkout in a named zone.
fn run(fixture: &Fixture, tz: &str, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(["--offline", "--path"])
        .arg(&fixture.repo)
        .args(["--since", &format!("@{T_SINCE}")])
        .args(args)
        .env("TZ", tz)
        .output()
        .expect("standup ran");
    assert!(
        out.status.success() || out.status.code() == Some(2),
        "standup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ---------------------------------------------------------------------------
// Civil time, written from scratch
// ---------------------------------------------------------------------------

/// `YYYY-MM-DD HH:MM` for a count of seconds since the epoch, treated as already
/// shifted into its local zone.
///
/// A second implementation of the calendar, deliberately: reusing the one under
/// test would let the same bug produce both sides of the comparison. This is the
/// standard days-to-civil reduction, valid for any date after 1 March 0000.
fn civil(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute) = (rem / 3_600, (rem % 3_600) / 60);

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Every `{epoch, local, zone, offset_seconds}` object anywhere in the JSON.
///
/// Walked rather than named field by field, so a stamp added in a new place
/// cannot escape these assertions by not being on a list.
fn stamps(value: &serde_json::Value, into: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.contains_key("epoch") && map.contains_key("local") {
                into.push(value.clone());
            }
            for nested in map.values() {
                stamps(nested, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                stamps(item, into);
            }
        }
        _ => {}
    }
}

fn all_stamps(json: &str) -> Vec<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    let mut found = Vec::new();
    stamps(&parsed, &mut found);
    assert!(
        found.len() >= 3,
        "the fixture should produce a generated_at, a window start and a commit: {}",
        found.len()
    );
    found
}

// ---------------------------------------------------------------------------
// The offset explains the string beside it
// ---------------------------------------------------------------------------

#[test]
fn every_stamp_carries_the_offset_its_local_time_was_rendered_in() {
    let fixture = Fixture::new("offsets");
    fixture.commits_around_the_window();

    // Whole hours, a half-hour zone, a three-quarter-hour zone, one west of
    // Greenwich and one past the date line. A rounded or hard-coded offset
    // survives none of these.
    for tz in [
        "UTC",
        "Europe/Berlin",
        "Asia/Kolkata",
        "Australia/Eucla",
        "America/St_Johns",
        "Pacific/Kiritimati",
    ] {
        for stamp in all_stamps(&run(&fixture, tz, &["--json"])) {
            let epoch = stamp["epoch"].as_i64().expect("an epoch");
            let local = stamp["local"].as_str().expect("a local string");
            let offset = stamp["offset_seconds"]
                .as_i64()
                .unwrap_or_else(|| panic!("{tz}: no offset on {stamp}"));
            assert_eq!(
                civil(epoch + offset),
                local,
                "{tz}: offset {offset} does not explain {local:?}"
            );
        }
    }
}

#[test]
fn the_offset_is_the_one_the_zone_uses_at_that_instant() {
    // Not "the machine's offset". Berlin is +0100 in January and +0200 in July,
    // and a digest whose window spans the change has to say so per stamp.
    let fixture = Fixture::new("dst");
    // 2026-01-15 12:00Z and 2026-07-15 12:00Z.
    let winter = 1_768_478_400_i64;
    let summer = 1_784_289_600_i64;
    fixture.write(&fixture.repo, "winter.txt", "cold\n");
    fixture.commit_all_at(&fixture.repo, winter, "winter work");
    fixture.write(&fixture.repo, "summer.txt", "warm\n");
    fixture.commit_all_at(&fixture.repo, summer, "summer work");

    let out = Command::new(BIN)
        .args(["--json", "--offline", "--path"])
        .arg(&fixture.repo)
        .args(["--since", &format!("@{}", winter - 86_400)])
        .env("TZ", "Europe/Berlin")
        .output()
        .expect("standup ran");
    let json = String::from_utf8_lossy(&out.stdout).to_string();

    let mut seen: Vec<(i64, i64)> = all_stamps(&json)
        .into_iter()
        .filter_map(|stamp| {
            let epoch = stamp["epoch"].as_i64()?;
            let offset = stamp["offset_seconds"].as_i64()?;
            (epoch == winter || epoch == summer).then_some((epoch, offset))
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen,
        vec![(winter, 3_600), (summer, 7_200)],
        "one digest, two offsets, each the one in force at its own instant:\n{json}"
    );
}

#[test]
fn a_named_zone_reaches_the_json_as_a_number() {
    let fixture = Fixture::new("kolkata");
    fixture.commits_around_the_window();
    let json = run(&fixture, "Asia/Kolkata", &["--json"]);
    let generated: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        generated["generated_at"]["offset_seconds"].as_i64(),
        Some(19_800),
        "+05:30 is 19800 seconds, and it is not expressible in whole hours"
    );
    assert_eq!(
        generated["window"]["since"]["offset_seconds"].as_i64(),
        Some(19_800),
        "the window boundary carries it too"
    );
}

// ---------------------------------------------------------------------------
// The reader's digest is unchanged
// ---------------------------------------------------------------------------

#[test]
fn the_human_digest_still_reads_in_prose() {
    let fixture = Fixture::new("prose");
    fixture.commits_around_the_window();
    for verb in ["--report", "--markdown", "--slack", "--html"] {
        let out = run(&fixture, "Asia/Kolkata", &[verb]);
        assert!(
            out.contains("+0530"),
            "{verb} should still print the zone as prose:\n{out}"
        );
        assert!(
            !out.contains("offset_seconds") && !out.contains("19800"),
            "{verb} must not leak the machine-readable half into a digest:\n{out}"
        );
    }
}

#[test]
fn the_json_still_reads_back_as_a_digest() {
    // The field is additive, and `--diff` deserialises the whole shape: a stamp
    // that gained a field the reader cannot parse would break comparisons rather
    // than help scripts.
    let fixture = Fixture::new("round-trip");
    fixture.commits_around_the_window();
    let json = run(&fixture, "Europe/Berlin", &["--json"]);
    let saved = fixture
        .repo
        .parent()
        .expect("temp root")
        .join("before.json");
    std::fs::write(&saved, &json).expect("wrote the snapshot");

    let compared = run(
        &fixture,
        "Europe/Berlin",
        &["--diff", saved.to_str().expect("utf-8 path")],
    );
    assert!(
        compared.contains("what changed between"),
        "the saved digest must still be readable:\n{compared}"
    );
}
