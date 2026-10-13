//! Dates formatted with moment.js format strings, the way Obsidian names
//! daily and periodic notes and fills `{{date:...}}` in templates.
//!
//! Follows moment's token table (<https://momentjs.com/docs/#/displaying/format/>)
//! in its default `en` locale, where a week starts on Sunday and week 1 is the
//! week holding January 1. Text in `[brackets]` is literal, and characters
//! that start no token are copied. A moment token this module doesn't
//! implement (ordinals other than `Do`, fractional seconds, time zones, the
//! era and `k` hours) is an error naming it, so a note is never given a
//! wrong name.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike, Weekday};

/// Every moment token, longest first so the longest match wins, and whether
/// this module renders it.
const TOKENS: &[(&str, bool)] = &[
    ("YYYYYY", false),
    ("NNNNN", false),
    ("SSSSSSSSS", false),
    ("SSSSSSSS", false),
    ("SSSSSSS", false),
    ("SSSSSS", false),
    ("SSSSS", false),
    ("YYYY", true),
    ("gggg", true),
    ("GGGG", true),
    ("MMMM", true),
    ("dddd", true),
    ("DDDD", true),
    ("DDDo", false),
    ("NNNN", false),
    ("SSSS", false),
    ("MMM", true),
    ("ddd", true),
    ("DDD", true),
    ("NNN", false),
    ("SSS", false),
    ("YY", true),
    ("gg", true),
    ("GG", true),
    ("MM", true),
    ("DD", true),
    ("dd", true),
    ("ww", true),
    ("WW", true),
    ("HH", true),
    ("hh", true),
    ("mm", true),
    ("ss", true),
    ("Do", true),
    ("Mo", false),
    ("Qo", false),
    ("do", false),
    ("wo", false),
    ("Wo", false),
    ("kk", false),
    ("zz", false),
    ("ZZ", false),
    ("NN", false),
    ("SS", false),
    ("Y", true),
    ("M", true),
    ("Q", true),
    ("D", true),
    ("d", true),
    ("e", true),
    ("E", true),
    ("w", true),
    ("W", true),
    ("A", true),
    ("a", true),
    ("H", true),
    ("h", true),
    ("m", true),
    ("s", true),
    ("X", true),
    ("y", false),
    ("k", false),
    ("S", false),
    ("z", false),
    ("Z", false),
    ("x", false),
    ("N", false),
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `at` formatted with the moment format `pattern`.
pub fn format(at: NaiveDateTime, pattern: &str) -> Result<String, String> {
    let date = at.date();
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(c) = rest.chars().next() {
        if c == '[' {
            match rest[1..].find(']') {
                Some(end) => {
                    out.push_str(&rest[1..1 + end]);
                    rest = &rest[end + 2..];
                }
                None => {
                    out.push_str(rest);
                    rest = "";
                }
            }
            continue;
        }
        let Some(&(token, supported)) = TOKENS.iter().find(|(token, _)| rest.starts_with(token))
        else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        };
        if !supported {
            return Err(format!(
                "the date format {pattern:?} uses the moment token {token:?}, which TurboVault doesn't support"
            ));
        }
        out.push_str(&render(token, at, date));
        rest = &rest[token.len()..];
    }
    Ok(out)
}

fn render(token: &str, at: NaiveDateTime, date: NaiveDate) -> String {
    let weekday = date.weekday();
    let (locale_year, locale_week) = locale_week(date);
    let iso = date.iso_week();
    let hour12 = match at.hour() % 12 {
        0 => 12,
        h => h,
    };
    match token {
        "YYYY" | "Y" => date.year().to_string(),
        "YY" => format!("{:02}", date.year().rem_euclid(100)),
        "gggg" => locale_year.to_string(),
        "gg" => format!("{:02}", locale_year.rem_euclid(100)),
        "GGGG" => iso.year().to_string(),
        "GG" => format!("{:02}", iso.year().rem_euclid(100)),
        "Q" => (date.month0() / 3 + 1).to_string(),
        "MMMM" => MONTHS[date.month0() as usize].to_string(),
        "MMM" => MONTHS[date.month0() as usize][..3].to_string(),
        "MM" => format!("{:02}", date.month()),
        "M" => date.month().to_string(),
        "DDDD" => format!("{:03}", date.ordinal()),
        "DDD" => date.ordinal().to_string(),
        "DD" => format!("{:02}", date.day()),
        "D" => date.day().to_string(),
        "Do" => ordinal(date.day()),
        "dddd" => weekday_name(weekday).to_string(),
        "ddd" => weekday_name(weekday)[..3].to_string(),
        "dd" => weekday_name(weekday)[..2].to_string(),
        "d" | "e" => weekday.num_days_from_sunday().to_string(),
        "E" => weekday.number_from_monday().to_string(),
        "ww" => format!("{locale_week:02}"),
        "w" => locale_week.to_string(),
        "WW" => format!("{:02}", iso.week()),
        "W" => iso.week().to_string(),
        "HH" => format!("{:02}", at.hour()),
        "H" => at.hour().to_string(),
        "hh" => format!("{hour12:02}"),
        "h" => hour12.to_string(),
        "mm" => format!("{:02}", at.minute()),
        "m" => at.minute().to_string(),
        "ss" => format!("{:02}", at.second()),
        "s" => at.second().to_string(),
        "A" => if at.hour() < 12 { "AM" } else { "PM" }.to_string(),
        "a" => if at.hour() < 12 { "am" } else { "pm" }.to_string(),
        "X" => at.and_utc().timestamp().to_string(),
        _ => String::new(),
    }
}

/// The `en` locale's week-year and week of `date`: weeks start on Sunday,
/// and week 1 is the one holding January 1.
pub fn locale_week(date: NaiveDate) -> (i32, u32) {
    let week_start = start_of_locale_week(date);
    let week_year = (week_start + chrono::Days::new(6)).year();
    let jan1 = NaiveDate::from_ymd_opt(week_year, 1, 1).unwrap_or(date);
    let first_week_start = start_of_locale_week(jan1);
    let week = (week_start - first_week_start).num_days() / 7 + 1;
    (week_year, week as u32)
}

/// The Sunday on or before `date`.
pub fn start_of_locale_week(date: NaiveDate) -> NaiveDate {
    date - chrono::Days::new(u64::from(date.weekday().num_days_from_sunday()))
}

/// Whether `pattern` numbers weeks the ISO way (`W`, `GGGG`, `E`), whose
/// weeks start on Monday.
pub fn uses_iso_weeks(pattern: &str) -> bool {
    let mut literal = false;
    pattern.chars().any(|c| match c {
        '[' => {
            literal = true;
            false
        }
        ']' => {
            literal = false;
            false
        }
        'W' | 'G' | 'E' => !literal,
        _ => false,
    })
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn ordinal(day: u32) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 5)
            .unwrap()
    }

    #[test]
    fn formats_the_tokens_vaults_name_notes_with() {
        let t = at(2026, 10, 13, 14, 7);
        for (pattern, expected) in [
            ("YYYY-MM-DD", "2026-10-13"),
            ("dddd, MMMM Do YYYY", "Tuesday, October 13th 2026"),
            ("ddd D MMM YY", "Tue 13 Oct 26"),
            ("YYYY/MM/YYYY-MM-DD", "2026/10/2026-10-13"),
            ("YYYY-[Q]Q", "2026-Q4"),
            ("gggg-[W]ww", "2026-W42"),
            ("GGGG-[W]WW", "2026-W42"),
            ("HH:mm", "14:07"),
            ("h:mm A", "2:07 PM"),
            ("DDDD [days in]", "286 days in"),
            ("[YYYY] YYYY", "YYYY 2026"),
            ("YYYYMMDDTHHmmss", "20261013T140705"),
        ] {
            assert_eq!(format(t, pattern).unwrap(), expected, "{pattern}");
        }
    }

    #[test]
    fn locale_and_iso_weeks_differ_where_they_should() {
        // 2027-01-01 is a Friday: in the en locale its week (from Sunday
        // 2026-12-27) is week 1 of 2027; by ISO it's week 53 of 2026.
        let t = at(2027, 1, 1, 0, 0);
        assert_eq!(format(t, "gggg-[W]ww").unwrap(), "2027-W01");
        assert_eq!(format(t, "GGGG-[W]WW").unwrap(), "2026-W53");
        // A Sunday starts a locale week but ends an ISO one.
        let sunday = at(2026, 10, 18, 0, 0);
        assert_eq!(format(sunday, "ww WW").unwrap(), "43 42");
    }

    #[test]
    fn refuses_tokens_it_cannot_render() {
        for pattern in ["Mo", "YYYY-MM-DD HH:mm:ss.SSS", "Z", "kk:mm"] {
            let error = format(at(2026, 1, 1, 0, 0), pattern).unwrap_err();
            assert!(error.contains("doesn't support"), "{pattern}: {error}");
        }
    }

    #[test]
    fn ordinals() {
        let days: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 31]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            days,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "31st"
            ]
        );
    }
}
