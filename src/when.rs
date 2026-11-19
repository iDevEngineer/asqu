//! Turns plain words like "Tuesday", "2 hours ago" or "before the signup change"
//! into a point on the Timeline.

use crate::store::LogEntry;
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Weekday};

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A specific entry number.
    Seq(i64),
    /// The last entry saved at or before this moment (unix seconds).
    AtOrBefore(i64),
    /// The newest entry whose request matches these words.
    Matching(String),
    /// The entry just before the newest one matching these words.
    BeforeMatching(String),
}

const FILLER: &[&str] = &["go", "back", "to", "the", "a", "an", "how", "it", "was", "on", "at", "last"];
const NOISE: &[&str] = &["the", "a", "an", "to", "of", "my", "our", "change", "changes", "edit", "edits", "update", "we", "made", "i"];

pub fn parse(words: &str, now: DateTime<Local>) -> Target {
    let text = words.trim().to_lowercase();
    let text = text.trim_start_matches('#');
    if let Ok(n) = text.parse::<i64>() {
        return Target::Seq(n);
    }

    let cleaned: Vec<&str> = text.split_whitespace().skip_while(|w| FILLER.contains(w)).collect();
    if let Some(rest) = cleaned.strip_prefix(&["before"]) {
        let rest = rest.join(" ");
        return match moment(&rest, now) {
            Some(Moment::Day(start)) => Target::AtOrBefore(start - 1),
            Some(Moment::Instant(t)) => Target::AtOrBefore(t - 1),
            None => Target::BeforeMatching(rest),
        };
    }

    let phrase = cleaned.join(" ");
    match moment(&phrase, now) {
        // "Tuesday" means how things stood at the end of Tuesday.
        Some(Moment::Day(start)) => Target::AtOrBefore(start + 86_400 - 1),
        Some(Moment::Instant(t)) => Target::AtOrBefore(t),
        None => Target::Matching(phrase),
    }
}

enum Moment {
    /// Start of a calendar day (unix seconds).
    Day(i64),
    Instant(i64),
}

fn moment(phrase: &str, now: DateTime<Local>) -> Option<Moment> {
    let words: Vec<&str> = phrase.split_whitespace().filter(|w| !FILLER.contains(w)).collect();
    let start_of = |days_ago: i64| {
        let date = now.date_naive() - Duration::days(days_ago);
        Local
            .from_local_datetime(&date.and_time(NaiveTime::MIN))
            .earliest()
            .map(|t| Moment::Day(t.timestamp()))
    };

    match words.as_slice() {
        ["today"] => start_of(0),
        ["yesterday"] => start_of(1),
        [day] => {
            let wd = weekday(day)?;
            // The most recent such day before today (1–7 days ago).
            let mut ago = (now.weekday().num_days_from_monday() as i64 - wd.num_days_from_monday() as i64 + 7) % 7;
            if ago == 0 {
                ago = 7;
            }
            start_of(ago)
        }
        [n, unit, "ago"] => {
            let n: i64 = match *n {
                "a" | "an" | "one" => 1,
                other => other.parse().ok()?,
            };
            let secs = match unit.trim_end_matches('s') {
                "minute" | "min" => 60,
                "hour" | "hr" => 3_600,
                "day" => 86_400,
                "week" => 604_800,
                _ => return None,
            };
            Some(Moment::Instant(now.timestamp() - n * secs))
        }
        [unit, "ago"] => moment(&format!("1 {unit} ago"), now),
        _ => None,
    }
}

fn weekday(word: &str) -> Option<Weekday> {
    let w = word.trim_end_matches('s'); // "tuesdays"
    Some(match w {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

/// Finds the entry a target points at. `entries` must be newest first.
pub fn resolve(entries: &[LogEntry], target: &Target) -> Option<i64> {
    match target {
        Target::Seq(n) => entries.iter().find(|e| e.seq == *n).map(|e| e.seq),
        Target::AtOrBefore(t) => entries.iter().find(|e| e.ts <= *t).map(|e| e.seq),
        Target::Matching(q) => best_match(entries, q).map(|e| e.seq),
        Target::BeforeMatching(q) => best_match(entries, q).map(|e| e.seq - 1).filter(|s| *s >= 1),
    }
}

/// The newest entry whose request shares the most meaningful words with the query.
fn best_match<'a>(entries: &'a [LogEntry], query: &str) -> Option<&'a LogEntry> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() > 1 && !NOISE.contains(&w.as_str()))
        .collect();
    if terms.is_empty() {
        return None;
    }
    let score = |e: &LogEntry| {
        // Search what was asked and Asqu's own description of the change.
        let text = format!("{} {}", e.request, e.summary.as_deref().unwrap_or_default()).to_lowercase();
        terms.iter().filter(|t| text.contains(t.as_str())).count()
    };
    // Undo entries quote the request they went back to, so they'd match too; skip them.
    let candidates: Vec<&LogEntry> = entries.iter().filter(|e| !e.request.starts_with("Went back to")).collect();
    let best = candidates.iter().map(|e| score(e)).max()?;
    if best == 0 {
        return None;
    }
    candidates.into_iter().find(|e| score(e) == best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash;

    fn now() -> DateTime<Local> {
        // Friday 16 Oct 2026, 15:00 local.
        Local.with_ymd_and_hms(2026, 10, 16, 15, 0, 0).unwrap()
    }

    fn day_end(y: i32, m: u32, d: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, 23, 59, 59).unwrap().timestamp()
    }

    #[test]
    fn numbers_and_times() {
        assert_eq!(parse("#3", now()), Target::Seq(3));
        assert_eq!(parse("12", now()), Target::Seq(12));
        assert_eq!(parse("go back to Tuesday", now()), Target::AtOrBefore(day_end(2026, 10, 13)));
        assert_eq!(parse("friday", now()), Target::AtOrBefore(day_end(2026, 10, 9)), "last Friday, not today");
        assert_eq!(parse("yesterday", now()), Target::AtOrBefore(day_end(2026, 10, 15)));
        assert_eq!(parse("2 hours ago", now()), Target::AtOrBefore(now().timestamp() - 7_200));
        assert_eq!(parse("an hour ago", now()), Target::AtOrBefore(now().timestamp() - 3_600));
        assert_eq!(parse("before yesterday", now()), Target::AtOrBefore(day_end(2026, 10, 14)));
    }

    #[test]
    fn words_become_searches() {
        assert_eq!(parse("before the signup change", now()), Target::BeforeMatching("the signup change".into()));
        assert_eq!(parse("dark mode", now()), Target::Matching("dark mode".into()));
    }

    fn entry(seq: i64, ts: i64, request: &str) -> LogEntry {
        LogEntry { seq, ts, hash: hash::ZERO, request: request.into(), actor: "t".into(), agent: None, edits: 1, summary: None }
    }

    #[test]
    fn resolving() {
        let log = vec![
            entry(4, 400, "Add dark mode toggle"),
            entry(3, 300, "Make signup faster"),
            entry(2, 200, "Fix typo on signup page"),
            entry(1, 100, "Start"),
        ];
        assert_eq!(resolve(&log, &Target::Seq(2)), Some(2));
        assert_eq!(resolve(&log, &Target::Seq(9)), None);
        assert_eq!(resolve(&log, &Target::AtOrBefore(250)), Some(2));
        assert_eq!(resolve(&log, &Target::AtOrBefore(50)), None);
        assert_eq!(resolve(&log, &Target::BeforeMatching("the signup change".into())), Some(2), "newest signup entry is #3");
        assert_eq!(resolve(&log, &Target::Matching("signup typo".into())), Some(2), "best match wins over newest");
        assert_eq!(resolve(&log, &Target::Matching("payments".into())), None);
    }

    #[test]
    fn undo_entries_never_match_words() {
        let log = vec![
            entry(3, 300, "Went back to #1 (\"Make signup faster\")"),
            entry(2, 200, "Make signup faster"),
            entry(1, 100, "Start"),
        ];
        assert_eq!(resolve(&log, &Target::BeforeMatching("the signup change".into())), Some(1));
    }
}
