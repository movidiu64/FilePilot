//! Date calendaristice fără dependențe (algoritmii „civil from days” ai lui Howard Hinnant).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ro,
    En,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
}

/// Zile de la 1970-01-01 pentru o dată calendaristică (calendar gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Data calendaristică pentru un număr de zile de la 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Secunde Unix pentru o dată și o oră în UTC.
pub fn timestamp(y: i64, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    days_from_civil(y, m, d) * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64
}

pub fn civil(ts: i64, utc_offset: i64) -> Civil {
    let t = ts + utc_offset;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    Civil { year, month, day, hour: rem / 3600, min: (rem % 3600) / 60, sec: rem % 60 }
}

pub fn month_name(month: u32, lang: Lang) -> &'static str {
    const RO: [&str; 12] = [
        "ianuarie", "februarie", "martie", "aprilie", "mai", "iunie", "iulie", "august",
        "septembrie", "octombrie", "noiembrie", "decembrie",
    ];
    const EN: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    let i = (month.clamp(1, 12) - 1) as usize;
    match lang {
        Lang::Ro => RO[i],
        Lang::En => EN[i],
    }
}

/// Formatează un moment după un format cu jetoane românești:
/// `AAAA` an, `AA` an pe 2 cifre, `LLLL` numele lunii, `LL` luna, `ZZ` ziua,
/// `HH` ora, `MM` minutul, `SS` secunda. Orice alt caracter rămâne neschimbat.
pub fn format(ts: i64, utc_offset: i64, fmt: &str, lang: Lang) -> String {
    let c = civil(ts, utc_offset);
    let en = english_tokens(fmt);
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut i = 0;
    while i < fmt.len() {
        let rest = &fmt[i..];
        let tok = |t: &str| rest.starts_with(t);
        let (text, len) = if en {
            if tok("YYYY") {
                (format!("{:04}", c.year), 4)
            } else if tok("YY") {
                (format!("{:02}", c.year.rem_euclid(100)), 2)
            } else if tok("MMMM") {
                (month_name(c.month, lang).to_string(), 4)
            } else if tok("MM") {
                (format!("{:02}", c.month), 2)
            } else if tok("DD") {
                (format!("{:02}", c.day), 2)
            } else if tok("HH") {
                (format!("{:02}", c.hour), 2)
            } else if tok("mm") {
                (format!("{:02}", c.min), 2)
            } else if tok("ss") {
                (format!("{:02}", c.sec), 2)
            } else {
                let ch = rest.chars().next().unwrap();
                (ch.to_string(), ch.len_utf8())
            }
        } else if tok("AAAA") {
            (format!("{:04}", c.year), 4)
        } else if tok("AA") {
            (format!("{:02}", c.year.rem_euclid(100)), 2)
        } else if tok("LLLL") {
            (month_name(c.month, lang).to_string(), 4)
        } else if tok("LL") {
            (format!("{:02}", c.month), 2)
        } else if tok("ZZ") {
            (format!("{:02}", c.day), 2)
        } else if tok("HH") {
            (format!("{:02}", c.hour), 2)
        } else if tok("MM") {
            (format!("{:02}", c.min), 2)
        } else if tok("SS") {
            (format!("{:02}", c.sec), 2)
        } else {
            let ch = rest.chars().next().unwrap();
            (ch.to_string(), ch.len_utf8())
        };
        out.push_str(&text);
        i += len;
    }
    out
}

/// Whether a format uses the English tokens (`YYYY-MM-DD HH-mm-ss`, `MMMM YYYY`) instead of
/// the Romanian ones (`AAAA-LL-ZZ HH-MM-SS`, `LLLL AAAA`). The two cannot be mixed: `MM` is
/// the month in an English format and the minutes in a Romanian one, as it always was. A
/// format that is only `MM` is the month (the minutes alone are of no use in a name).
pub fn english_tokens(fmt: &str) -> bool {
    ["YY", "DD", "MMMM", "mm", "ss"].iter().any(|t| fmt.contains(t)) || fmt.trim() == "MM"
}
