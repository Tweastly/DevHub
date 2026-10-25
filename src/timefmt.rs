//! Dates without a date crate: RFC 3339 parsing (Docker, GitHub) and French formatting.

use crate::db::now;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// "2026-10-25T18:27:29.260294586Z" or "+02:00" offsets → unix seconds. Docker's zero
/// date ("0001-01-01T00:00:00Z") and anything unparsable give None.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if y < 1970 {
        return None;
    }
    let mut ts = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;
    let tail = &s[19..];
    let tz = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = tz.chars().next().filter(|c| *c == '+' || *c == '-') {
        let off = tz.get(1..3)?.parse::<i64>().ok()? * 3600 + tz.get(4..6)?.parse::<i64>().ok()? * 60;
        ts -= if sign == '+' { off } else { -off };
    }
    Some(ts)
}

pub fn ymd_hm(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86400));
    let s = ts.rem_euclid(86400);
    format!("{y}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
}

pub fn hm(ts: i64) -> String {
    let s = ts.rem_euclid(86400);
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

const MONTHS: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
const MONTHS_LONG: [&str; 12] =
    ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
const WEEKDAYS: [&str; 7] = ["jeudi", "vendredi", "samedi", "dimanche", "lundi", "mardi", "mercredi"];

/// "12 oct. 2026"
pub fn date_short(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86400));
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// Day header for the journal: "AUJOURD'HUI · DIMANCHE 25 OCTOBRE".
pub fn day_header(ts: i64) -> String {
    let day = ts.div_euclid(86400);
    let today = now().div_euclid(86400);
    let (_, m, d) = civil_from_days(day);
    let name = format!("{} {d} {}", WEEKDAYS[day.rem_euclid(7) as usize], MONTHS_LONG[(m - 1) as usize]);
    let prefix = match today - day {
        0 => "Aujourd'hui · ",
        1 => "Hier · ",
        _ => "",
    };
    format!("{prefix}{name}").to_uppercase()
}

/// "il y a 4 min", "hier 18:20", "12 oct. 2026".
pub fn relative(ts: i64) -> String {
    let d = now() - ts;
    let day = |t: i64| t.div_euclid(86400);
    match d {
        d if d < 45 => "à l'instant".into(),
        d if d < 3600 => format!("il y a {} min", (d + 30) / 60),
        d if d < 6 * 3600 => format!("il y a {} h", d / 3600),
        _ if day(now()) == day(ts) => format!("aujourd'hui {}", hm(ts)),
        _ if day(now()) - day(ts) == 1 => format!("hier {}", hm(ts)),
        d if d < 7 * 86400 => format!("il y a {} j", d / 86400),
        _ => date_short(ts),
    }
}

/// "2 j 4 h", "3 h 12 min", "8 min".
pub fn duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d} j {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{} min", m.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_and_github_dates() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-25T18:27:29.260294586Z"), Some(1792952849));
        assert_eq!(parse_rfc3339("2026-10-25T20:27:29+02:00"), Some(1792952849));
        assert_eq!(parse_rfc3339("0001-01-01T00:00:00Z"), None);
        assert_eq!(ymd_hm(1792952849), "2026-10-25 18:27");
    }
}
