//! Values taken from a file's text: a date, a word from a list, a pattern or a regular
//! expression, at a chosen occurrence, optionally right after or before a label
//! (`Invoice no.` → the number that follows it).

use crate::regex::{Class, Node, Regex, RegexError};
use crate::timefmt::{self, Lang};

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractKind {
    /// A calendar date in the usual forms (`15.11.2026`, `2026-11-15`, `15 noiembrie 2026`,
    /// `November 15, 2026`…), written out with `format` (`AAAA-LL-ZZ`).
    Date { format: String },
    /// One of these words or phrases, as written in the list. Whole words only.
    Words(Vec<String>),
    /// A simple pattern: `#` digit, `@` letter, `?` any character, `%` a number,
    /// `*` any text on the same line; spaces match any whitespace.
    Pattern(String),
    /// A regular expression; the value is its first group, or the whole match.
    Regex(String),
}

/// Which match to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occurrence {
    /// 1 = the first.
    Nth(u32),
    Last,
}

/// How a variable takes its value from text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    pub kind: ExtractKind,
    pub occurrence: Occurrence,
    /// Text that must come right before the value (spaces in between are allowed).
    pub before: String,
    /// Text that must come right after the value.
    pub after: String,
}

impl Default for Extract {
    fn default() -> Self {
        Extract { kind: ExtractKind::Words(Vec::new()), occurrence: Occurrence::Nth(1), before: String::new(), after: String::new() }
    }
}

/// One value found in the text: its position (bytes) and the value to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub value: String,
}

/// A checked, ready-to-run [`Extract`].
pub struct Extractor {
    finders: Vec<(Regex, Finder)>,
    occurrence: Occurrence,
    before: String,
    after: String,
    lang: Lang,
}

#[derive(Clone)]
enum Finder {
    Plain,
    Word(String),
    Date(DateForm, String),
}

#[derive(Clone, Copy)]
enum DateForm {
    /// 15.11.2026, 15/11/2026, 15-11-2026 (day first; month first only when the day
    /// cannot be a month).
    DayMonthYear,
    /// 2026-11-15, 2026.11.15, 2026/11/15
    YearMonthDay,
    /// 15 noiembrie 2026, 15. Nov 2026
    DayNameYear,
    /// November 15, 2026
    NameDayYear,
}

const MONTHS: [(&str, u32); 31] = [
    ("ianuarie", 1), ("februarie", 2), ("martie", 3), ("aprilie", 4), ("mai", 5), ("iunie", 6),
    ("iulie", 7), ("august", 8), ("septembrie", 9), ("octombrie", 10), ("noiembrie", 11), ("decembrie", 12),
    ("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6),
    ("july", 7), ("september", 9), ("october", 10), ("november", 11), ("december", 12),
    ("sept", 9), ("ian", 1), ("iun", 6), ("iul", 7), ("noi", 11), ("nov", 11), ("feb", 2), ("dec", 12),
];

/// Three-letter forms not covered above.
const SHORT: [(&str, u32); 8] = [("jan", 1), ("mar", 3), ("apr", 4), ("jun", 6), ("jul", 7), ("aug", 8), ("sep", 9), ("oct", 10)];

fn month_number(name: &str) -> Option<u32> {
    let n = name.trim_end_matches('.').to_lowercase();
    MONTHS.iter().chain(SHORT.iter()).find(|(m, _)| *m == n).map(|(_, k)| *k)
}

fn month_alternation() -> String {
    let mut names: Vec<&str> = MONTHS.iter().chain(SHORT.iter()).map(|(m, _)| *m).collect();
    // Longest first, so `noiembrie` wins over `noi`.
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.dedup();
    names.join("|")
}

impl Extractor {
    pub fn new(ex: &Extract, lang: Lang) -> Result<Extractor, RegexError> {
        let finders = match &ex.kind {
            ExtractKind::Regex(p) => {
                if p.trim().is_empty() {
                    return Err(RegexError("the expression is empty".into()));
                }
                vec![(Regex::new(p)?, Finder::Plain)]
            }
            ExtractKind::Pattern(p) => {
                if p.trim().is_empty() {
                    return Err(RegexError("the pattern is empty".into()));
                }
                vec![(pattern(p)?, Finder::Plain)]
            }
            ExtractKind::Words(words) => {
                let mut f = Vec::new();
                for w in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
                    let lit: String = w.chars().map(|c| if c.is_alphanumeric() || c == ' ' { c.to_string() } else { format!("\\{c}") }).collect();
                    let lit = lit.split_whitespace().collect::<Vec<_>>().join("\\s+");
                    // Whole words: a boundary on each side that starts or ends with a letter or digit.
                    let edge = |c: Option<char>| if c.is_some_and(char::is_alphanumeric) { "\\b" } else { "" };
                    let re = format!("{}{lit}{}", edge(w.chars().next()), edge(w.chars().last()));
                    f.push((Regex::new(&re)?, Finder::Word(w.to_string())));
                }
                if f.is_empty() {
                    return Err(RegexError("the list is empty".into()));
                }
                f
            }
            ExtractKind::Date { format } => {
                let m = month_alternation();
                let out = if format.trim().is_empty() { "AAAA-LL-ZZ".to_string() } else { format.clone() };
                vec![
                    (Regex::new(r"\b(\d{1,2})[./-](\d{1,2})[./-](\d{4})\b")?, Finder::Date(DateForm::DayMonthYear, out.clone())),
                    // `T` for ISO timestamps (2026-11-15T10:00).
                    (Regex::new(r"\b(\d{4})[./-](\d{1,2})[./-](\d{1,2})(?:\b|T)")?, Finder::Date(DateForm::YearMonthDay, out.clone())),
                    (Regex::new(&format!(r"\b(\d{{1,2}})\.?\s+({m})\.?,?\s+(\d{{4}})\b"))?, Finder::Date(DateForm::DayNameYear, out.clone())),
                    (Regex::new(&format!(r"\b({m})\.?\s+(\d{{1,2}}),?\s+(\d{{4}})\b"))?, Finder::Date(DateForm::NameDayYear, out)),
                ]
            }
        };
        if matches!(ex.occurrence, Occurrence::Nth(0)) {
            return Err(RegexError("the occurrence starts at 1".into()));
        }
        Ok(Extractor { finders, occurrence: ex.occurrence, before: ex.before.trim().to_lowercase(), after: ex.after.trim().to_lowercase(), lang })
    }

    /// Every value in the text that meets the before/after requirements, in order.
    pub fn all(&self, text: &str) -> Vec<Found> {
        let mut found: Vec<Found> = Vec::new();
        for (re, finder) in &self.finders {
            for m in re.find_iter(text) {
                let (s, e) = match finder {
                    Finder::Plain => m.value_range(),
                    _ => (m.start, m.end),
                };
                let value = match finder {
                    Finder::Plain => text[s..e].trim().to_string(),
                    Finder::Word(w) => w.clone(),
                    Finder::Date(form, fmt) => {
                        let g = |k: usize| m.groups[k].map(|(a, b)| &text[a..b]).unwrap_or("");
                        match date_value(*form, g(0), g(1), g(2), fmt, self.lang) {
                            Some(v) => v,
                            None => continue,
                        }
                    }
                };
                if value.is_empty() || !self.fits(text, s, e) {
                    continue;
                }
                found.push(Found { start: s, end: e, value });
            }
        }
        // Several finders (dates, words): merge by position, dropping overlaps.
        found.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
        let mut out: Vec<Found> = Vec::new();
        for f in found {
            if out.last().is_none_or(|l| f.start >= l.end) {
                out.push(f);
            }
        }
        out
    }

    /// The value at the chosen occurrence.
    pub fn find(&self, text: &str) -> Option<String> {
        let all = self.all(text);
        match self.occurrence {
            Occurrence::Nth(n) => all.into_iter().nth(n as usize - 1),
            Occurrence::Last => all.into_iter().last(),
        }
        .map(|f| f.value)
    }

    fn fits(&self, text: &str, start: usize, end: usize) -> bool {
        let before_ok = self.before.is_empty() || {
            let head = text[..start].trim_end();
            ends_with_ci(head, &self.before)
        };
        let after_ok = self.after.is_empty() || {
            let tail = text[end..].trim_start();
            starts_with_ci(tail, &self.after)
        };
        before_ok && after_ok
    }
}

fn starts_with_ci(hay: &str, needle_lower: &str) -> bool {
    let mut h = hay.chars().flat_map(char::to_lowercase);
    needle_lower.chars().all(|n| h.next() == Some(n))
}

fn ends_with_ci(hay: &str, needle_lower: &str) -> bool {
    let mut h = hay.chars().rev().flat_map(char::to_lowercase);
    needle_lower.chars().rev().all(|n| h.next() == Some(n))
}

fn date_value(form: DateForm, a: &str, b: &str, c: &str, fmt: &str, lang: Lang) -> Option<String> {
    let num = |s: &str| s.parse::<u32>().ok();
    let (y, m, d) = match form {
        DateForm::DayMonthYear => {
            let (x, y2) = (num(a)?, num(b)?);
            // Day first, unless only the other way round makes a valid date (11/25/2026).
            if y2 > 12 && x <= 12 { (num(c)?, x, y2) } else { (num(c)?, y2, x) }
        }
        DateForm::YearMonthDay => (num(a)?, num(b)?, num(c)?),
        DateForm::DayNameYear => (num(c)?, month_number(b)?, num(a)?),
        DateForm::NameDayYear => (num(c)?, month_number(a)?, num(b)?),
    };
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) || !(1900..=2200).contains(&y) {
        return None;
    }
    let ts = timefmt::timestamp(i64::from(y), m, d, 0, 0, 0);
    Some(timefmt::format(ts, 0, fmt, lang))
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Compiles the simple pattern syntax to a regular expression.
fn pattern(p: &str) -> Result<Regex, RegexError> {
    let mut items = Vec::new();
    let mut chars = p.trim().chars().peekable();
    while let Some(c) = chars.next() {
        items.push(match c {
            '#' => Node::Class(Class::digit()),
            '@' => Node::Class(Class::letter()),
            '?' => Node::Class(Class::not_newline()),
            '*' => Node::Repeat { node: Box::new(Node::Class(Class::not_newline())), min: 0, max: None, greedy: false },
            '%' => {
                // 1234, 1.234,56, 12,5
                let digits = || Node::Repeat { node: Box::new(Node::Class(Class::digit())), min: 1, max: None, greedy: true };
                let sep = Node::Alt(vec![Node::Char('.'), Node::Char(',')]);
                Node::Concat(vec![
                    digits(),
                    Node::Repeat { node: Box::new(Node::Concat(vec![sep, digits()])), min: 0, max: None, greedy: true },
                ])
            }
            c if c.is_whitespace() => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                Node::Repeat { node: Box::new(Node::Class(Class::space())), min: 1, max: None, greedy: true }
            }
            '\\' => match chars.next() {
                Some(n) => Node::Char(n),
                None => return Err(RegexError("\\ at the end".into())),
            },
            c => Node::Char(c),
        });
    }
    Regex::from_node(&Node::Concat(items), 0)
}
