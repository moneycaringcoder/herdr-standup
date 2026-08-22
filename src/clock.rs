//! Local time, and nothing else.
//!
//! The roadmap calls an ambiguous timestamp in a daily digest a bug, so every
//! instant the plugin prints goes through [`stamp`] and arrives carrying its own
//! zone. There is no time crate: `localtime_r` already knows the user's zone,
//! including the abbreviation and the offset, and it is the same zone database
//! git formats `--date=format-local` against, so the two agree by construction.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::Stamp;

/// Seconds since the Unix epoch, now.
pub fn now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_secs() as i64,
        // Before 1970. Impossible in practice; saturating beats panicking in a
        // digest tool.
        Err(err) => -(err.duration().as_secs() as i64),
    }
}

/// An epoch second, rendered in the local zone.
pub fn stamp(epoch: i64) -> Stamp {
    let (local, zone) = format_local(epoch);
    Stamp { epoch, local, zone }
}

/// Local midnight at the start of the day containing `epoch`.
///
/// Computed by asking `localtime_r` for the civil time and subtracting the
/// seconds elapsed since midnight, which is correct across every offset the
/// zone database knows, including the half-hour and three-quarter-hour ones.
///
/// A day on which the clock jumps forward at midnight has no 00:00 at all. In
/// that case the subtraction lands on the last instant of the previous day,
/// which is the conservative direction for a window boundary: the digest starts
/// slightly early rather than dropping the first commits of the day.
pub fn midnight(epoch: i64) -> i64 {
    match civil(epoch) {
        Some(tm) => {
            let since_midnight =
                i64::from(tm.tm_hour) * 3_600 + i64::from(tm.tm_min) * 60 + i64::from(tm.tm_sec);
            epoch - since_midnight
        }
        // No local zone available: fall back to UTC midnight. Reported by the
        // caller through the window it prints, so it is never silent.
        None => epoch - epoch.rem_euclid(86_400),
    }
}

/// Local midnight at the start of the **ISO week** — Monday — containing
/// `epoch`.
///
/// Monday because ISO 8601 says so and because a locale-derived week start
/// would make the same command mean different windows on two machines. It is a
/// stated convention rather than a guess, and the header prints the instant it
/// resolved to, so a reader can check the boundary rather than trust it.
pub fn week_start(epoch: i64) -> i64 {
    // `tm_wday` is 0 for Sunday, so Monday is 1 and the distance back to it is
    // `(wday + 6) % 7`.
    let back = match civil(epoch) {
        Some(tm) => i64::from((tm.tm_wday + 6) % 7),
        None => return midnight(epoch),
    };
    days_before(epoch, back)
}

/// Local midnight on the first day of the calendar month containing `epoch`.
pub fn month_start(epoch: i64) -> i64 {
    let back = match civil(epoch) {
        Some(tm) => i64::from(tm.tm_mday - 1),
        None => return midnight(epoch),
    };
    days_before(epoch, back)
}

/// Midnight `days` local days before the day containing `epoch`.
///
/// Subtracting whole days from a *midnight* would drift on a day the clock
/// moves: the result is an hour either side of midnight, and one of those is the
/// previous day. So it lands near local noon of the target day first and takes
/// that day's midnight, which is right for every offset the zone database knows
/// — the largest transition ever used is two hours, and noon has twelve of slack
/// in both directions.
fn days_before(epoch: i64, days: i64) -> i64 {
    let noon = midnight(epoch) + 43_200 - days * 86_400;
    midnight(noon)
}

/// `("2026-08-15 09:12", "CEST +0200")`.
fn format_local(epoch: i64) -> (String, String) {
    let Some(tm) = civil(epoch) else {
        return (format!("epoch {epoch}"), "unknown zone".to_string());
    };
    let local = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    );
    let offset = {
        let total = tm.tm_gmtoff;
        let sign = if total < 0 { '-' } else { '+' };
        let abs = total.abs();
        format!("{sign}{:02}{:02}", abs / 3_600, (abs % 3_600) / 60)
    };
    let zone = match zone_name(&tm) {
        Some(name) => format!("{name} {offset}"),
        None => offset,
    };
    (local, zone)
}

#[cfg(unix)]
fn civil(epoch: i64) -> Option<libc::tm> {
    // SAFETY: `localtime_r` writes a complete `struct tm` through the out
    // pointer and returns null on failure. The zeroed value is never read
    // unless the call reports success.
    unsafe {
        let time = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            None
        } else {
            Some(tm)
        }
    }
}

#[cfg(unix)]
fn zone_name(tm: &libc::tm) -> Option<String> {
    if tm.tm_zone.is_null() {
        return None;
    }
    // SAFETY: `tm_zone` points at a NUL-terminated static string owned by the C
    // library, valid until the next call that changes the zone. We copy it out
    // immediately.
    let name = unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }
        .to_string_lossy()
        .into_owned();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(not(unix))]
fn civil(_epoch: i64) -> Option<Tm> {
    None
}

#[cfg(not(unix))]
struct Tm;

#[cfg(not(unix))]
fn zone_name(_tm: &Tm) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the arithmetic rather than the machine's zone: whatever zone the
    /// test host is in, midnight must be at or before the instant, no more than
    /// a day earlier, and must itself land on a 00:00 local wall clock.
    #[test]
    fn midnight_is_the_start_of_the_local_day() {
        for probe in [0_i64, 1_786_831_294, 1_600_000_000, 2_000_000_000] {
            let start = midnight(probe);
            assert!(start <= probe, "midnight {start} after {probe}");
            assert!(probe - start < 86_400, "midnight {start} too far before");
            let stamp = stamp(start);
            assert!(
                stamp.local.ends_with(" 00:00") || stamp.zone == "unknown zone",
                "midnight did not land on 00:00: {}",
                stamp.full()
            );
        }
    }

    /// Pins the arithmetic, not the machine's zone: whatever zone the host is
    /// in, the week must start on a Monday at 00:00, at or before the instant,
    /// and no more than seven days earlier.
    #[test]
    fn a_week_starts_on_the_monday_before_it() {
        for probe in [
            0_i64,
            1_786_831_294,
            1_600_000_000,
            2_000_000_000,
            // Straddles a European DST transition, which is where naive
            // day-subtraction lands an hour into the wrong day.
            1_761_440_000,
        ] {
            let start = week_start(probe);
            assert!(start <= probe, "week start {start} after {probe}");
            assert!(
                probe - start < 7 * 86_400,
                "week start {start} more than a week before {probe}"
            );
            let Some(tm) = civil(start) else {
                continue; // No zone database on this host; nothing to check.
            };
            assert_eq!(
                tm.tm_wday,
                1,
                "week started on weekday {} rather than Monday: {}",
                tm.tm_wday,
                stamp(start).full()
            );
            assert_eq!(
                (tm.tm_hour, tm.tm_min, tm.tm_sec),
                (0, 0, 0),
                "week did not start at midnight: {}",
                stamp(start).full()
            );
        }
    }

    #[test]
    fn a_month_starts_on_its_first_day() {
        for probe in [
            0_i64,
            1_786_831_294,
            1_600_000_000,
            2_000_000_000,
            1_761_440_000,
        ] {
            let start = month_start(probe);
            assert!(start <= probe, "month start {start} after {probe}");
            assert!(
                probe - start < 31 * 86_400,
                "month start {start} more than a month before {probe}"
            );
            let Some(tm) = civil(start) else {
                continue;
            };
            assert_eq!(
                tm.tm_mday,
                1,
                "month started on day {} rather than the 1st: {}",
                tm.tm_mday,
                stamp(start).full()
            );
            assert_eq!(
                (tm.tm_hour, tm.tm_min, tm.tm_sec),
                (0, 0, 0),
                "month did not start at midnight: {}",
                stamp(start).full()
            );
        }
    }

    /// A boundary is idempotent: asking for the start of the week containing a
    /// week start gives the same instant back. Cheap, and it catches an
    /// off-by-one that would otherwise only show on one weekday.
    #[test]
    fn the_boundaries_are_their_own_starts() {
        for probe in [1_786_831_294_i64, 1_600_000_000, 1_761_440_000] {
            let week = week_start(probe);
            assert_eq!(week_start(week), week, "week start moved on the second ask");
            let month = month_start(probe);
            assert_eq!(
                month_start(month),
                month,
                "month start moved on the second ask"
            );
        }
    }

    #[test]
    fn a_stamp_always_names_its_zone() {
        let stamp = stamp(1_786_831_294);
        assert!(stamp.full().starts_with(&stamp.local));
        assert!(!stamp.zone.is_empty());
        // The offset is always present, with a sign and four digits.
        let offset = stamp.zone.rsplit(' ').next().unwrap();
        assert_eq!(offset.len(), 5, "malformed offset in {:?}", stamp.zone);
        assert!(offset.starts_with('+') || offset.starts_with('-'));
    }

    #[test]
    fn now_is_plausible() {
        // After 2020 and before 2100: catches a units mix-up (millis for secs).
        let now = now();
        assert!(now > 1_577_836_800, "clock before 2020: {now}");
        assert!(now < 4_102_444_800, "clock after 2100: {now}");
    }
}
