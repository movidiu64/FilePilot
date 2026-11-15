//! The regular expression engine and the values taken from text.

use org_core::extract::*;
use org_core::regex::Regex;
use org_core::Lang;

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn first(p: &str, text: &str) -> Option<String> {
    re(p).find_at(text, 0).map(|m| text[m.start..m.end].to_string())
}

fn all(p: &str, text: &str) -> Vec<String> {
    re(p).find_iter(text).map(|m| text[m.start..m.end].to_string()).collect()
}

#[test]
fn literals_classes_and_quantifiers() {
    assert_eq!(first("abc", "xxabcxx").as_deref(), Some("abc"));
    assert_eq!(first(r"\d+", "Invoice 2026/0042").as_deref(), Some("2026"));
    assert_eq!(first(r"\d{4}-\d{2}", "on 2026-11-15").as_deref(), Some("2026-11"));
    assert_eq!(first(r"[A-F0-9]{2,}", "id: zz 3fa9 ").as_deref(), Some("3fa9"));
    assert_eq!(first(r"[^ ]+@[^ ]+", "write to ana@example.com today").as_deref(), Some("ana@example.com"));
    assert_eq!(first(r"colou?r", "COLOR").as_deref(), Some("COLOR"), "case is ignored");
    assert_eq!(first(r"\w+\s\w+", "  hello   big world").as_deref(), Some("big world"));
    assert_eq!(first(r"a.c", "a\nc abc").as_deref(), Some("abc"), ". does not cross lines");
    assert_eq!(first(r"x{3}", "xx xxx").as_deref(), Some("xxx"));
    assert_eq!(first(r"\{a\}", "{a}").as_deref(), Some("{a}"));
    assert_eq!(first(r"a{,2}", "a{,2}").as_deref(), Some("a{,2}"), "not a repetition: a literal brace");
    assert_eq!(first(r"ș+", "ȘȘș").as_deref(), Some("ȘȘș"), "Romanian letters fold too");
    assert_eq!(first(r"[ă-ț]+", "ĂȚ").as_deref(), Some("ĂȚ"));
}

#[test]
fn leftmost_first_greedy_and_lazy() {
    assert_eq!(first("a|ab", "ab").as_deref(), Some("a"), "the first alternative wins");
    assert_eq!(first("ab|a", "ab").as_deref(), Some("ab"));
    assert_eq!(first("<.+>", "<a><b>").as_deref(), Some("<a><b>"));
    assert_eq!(first("<.+?>", "<a><b>").as_deref(), Some("<a>"));
    assert_eq!(first("a*", "bbb").as_deref(), Some(""), "empty match at the start");
    assert_eq!(all(r"\d+", "1 22 333"), vec!["1", "22", "333"]);
    assert_eq!(all("x*", "ab").len(), 3, "empty matches advance by one character");
}

#[test]
fn anchors_and_boundaries() {
    assert_eq!(all(r"^\w+", "first line\nsecond line"), vec!["first", "second"]);
    assert_eq!(all(r"\w+$", "first line\r\nsecond line"), vec!["line", "line"]);
    assert_eq!(all(r"\bcat\b", "cat concat cat."), vec!["cat", "cat"]);
    assert_eq!(all(r"\Bcat", "cat concat"), vec!["cat"]);
}

#[test]
fn groups() {
    let r = re(r"Invoice no\.?\s*(\d+)(?:/(\d+))?");
    let t = "Invoice no. 4521/26 and Invoice no 7";
    let ms: Vec<_> = r.find_iter(t).collect();
    assert_eq!(ms.len(), 2);
    assert_eq!(ms[0].groups.len(), 2);
    let g = |m: &org_core::regex::Match, k: usize| m.groups[k].map(|(a, b)| &t[a..b]);
    assert_eq!((g(&ms[0], 0), g(&ms[0], 1)), (Some("4521"), Some("26")));
    assert_eq!((g(&ms[1], 0), g(&ms[1], 1)), (Some("7"), None));
    assert_eq!(&t[ms[0].value_range().0..ms[0].value_range().1], "4521");
}

#[test]
fn bad_expressions_are_refused_not_run() {
    for p in ["(abc", "abc)", "[abc", "*a", "a**", r"\q", "a{2,1}", "a{5000}", "(?=x)", "x{1000}{1000}"] {
        assert!(Regex::new(p).is_err(), "{p} should be refused");
    }
}

#[test]
fn hostile_expressions_stay_fast() {
    // Catastrophic for backtracking engines; linear here.
    let text = "a".repeat(20_000) + "!";
    let t = std::time::Instant::now();
    assert!(re("(a+)+$").find_at(&text, 0).is_none());
    assert!(re("(a|aa)*b").find_at(&text, 0).is_none());
    assert!(t.elapsed().as_secs() < 5, "took {:?}", t.elapsed());
}

fn ex(kind: ExtractKind) -> Extract {
    Extract { kind, ..Extract::default() }
}

fn find(e: &Extract, text: &str) -> Option<String> {
    Extractor::new(e, Lang::Ro).unwrap().find(text)
}

#[test]
fn dates_in_the_usual_forms() {
    let e = ex(ExtractKind::Date { format: "AAAA-LL-ZZ".into() });
    let cases = [
        ("Data: 15.11.2026", "2026-11-15"),
        ("emisă 5/3/2026", "2026-03-05"),
        ("due 11/25/2026", "2026-11-25"),
        ("ISO 2026-11-15T10:00", "2026-11-15"),
        ("din 15 noiembrie 2026", "2026-11-15"),
        ("din 3 Noi. 2026", "2026-11-03"),
        ("on 15 Nov 2026", "2026-11-15"),
        ("dated November 15, 2026", "2026-11-15"),
        ("Sept 9 2026", "2026-09-09"),
        ("29.02.2028", "2028-02-29"),
    ];
    for (text, want) in cases {
        assert_eq!(find(&e, text).as_deref(), Some(want), "{text}");
    }
    for text in ["31.02.2026", "29.02.2027", "15.13.2026", "tel 0722.123.456", "1.2.3.4", "version 10.2026"] {
        assert_eq!(find(&e, text), None, "{text}");
    }
}

#[test]
fn date_format_and_language() {
    let e = ex(ExtractKind::Date { format: "LLLL AAAA".into() });
    assert_eq!(Extractor::new(&e, Lang::Ro).unwrap().find("15.11.2026").as_deref(), Some("noiembrie 2026"));
    assert_eq!(Extractor::new(&e, Lang::En).unwrap().find("15.11.2026").as_deref(), Some("November 2026"));
    let e = ex(ExtractKind::Date { format: String::new() });
    assert_eq!(find(&e, "15.11.2026").as_deref(), Some("2026-11-15"), "empty format = AAAA-LL-ZZ");
}

#[test]
fn occurrence_and_labels() {
    let text = "Data emiterii: 01.11.2026\nData scadentă: 30.11.2026\nPlătit la 05.12.2026";
    let mut e = ex(ExtractKind::Date { format: "ZZ.LL".into() });
    assert_eq!(find(&e, text).as_deref(), Some("01.11"));
    e.occurrence = Occurrence::Nth(2);
    assert_eq!(find(&e, text).as_deref(), Some("30.11"));
    e.occurrence = Occurrence::Last;
    assert_eq!(find(&e, text).as_deref(), Some("05.12"));
    e.occurrence = Occurrence::Nth(9);
    assert_eq!(find(&e, text), None);
    e.occurrence = Occurrence::Nth(1);
    e.before = "scadentă:".into();
    assert_eq!(find(&e, text).as_deref(), Some("30.11"));
    e.before = "SCADENTĂ".into();
    assert_eq!(find(&e, text), None, "the label must come right before (only spaces between)");
    e.before = String::new();
    e.after = "plătit".into();
    assert_eq!(find(&e, text).as_deref(), Some("30.11"), "the date followed by the label");
}

#[test]
fn words_from_a_list() {
    let e = ex(ExtractKind::Words(vec!["Enel".into(), "E.ON".into(), "Digi".into(), "Orange Romania".into()]));
    assert_eq!(find(&e, "Factură ENEL Energie").as_deref(), Some("Enel"), "the value is written as in the list");
    assert_eq!(find(&e, "furnizor: e.on energie").as_deref(), Some("E.ON"));
    assert_eq!(find(&e, "Orange\nRomania SA").as_deref(), Some("Orange Romania"));
    assert_eq!(find(&e, "Digital services"), None, "whole words only");
    let all = Extractor::new(&e, Lang::Ro).unwrap().all("Digi, apoi Enel, apoi Digi");
    assert_eq!(all.iter().map(|f| f.value.as_str()).collect::<Vec<_>>(), vec!["Digi", "Enel", "Digi"], "in text order");
}

#[test]
fn simple_patterns() {
    let p = |pat: &str, text: &str| find(&ex(ExtractKind::Pattern(pat.into())), text);
    assert_eq!(p("INV-####", "ref INV-0042 / INV-12").as_deref(), Some("INV-0042"));
    assert_eq!(p("@@ ######", "plates: CJ 123456").as_deref(), Some("CJ 123456"));
    assert_eq!(p("Total: % lei", "Total:  1.234,56 lei").as_deref(), Some("Total:  1.234,56 lei"));
    assert_eq!(p("cod ?-##", "cod A-12").as_deref(), Some("cod A-12"));
    assert_eq!(p("Client: *,", "Client: SC Exemplu SRL, CUI 1").as_deref(), Some("Client: SC Exemplu SRL,"));
    assert_eq!(p(r"\#%", "item #42").as_deref(), Some("#42"));
    // With a label, the pattern only needs to describe the value.
    let mut e = ex(ExtractKind::Pattern("%".into()));
    e.before = "Total:".into();
    assert_eq!(find(&e, "Subtotal 10\nTotal: 1.234,56 lei").as_deref(), Some("1.234,56"));
}

#[test]
fn regex_values_use_the_first_group() {
    let mut e = ex(ExtractKind::Regex(r"Nr\.?\s*(\d+)".into()));
    assert_eq!(find(&e, "Factura Nr. 4521 din").as_deref(), Some("4521"));
    e.kind = ExtractKind::Regex(r"[A-Z]{2}\d{2}".into());
    assert_eq!(find(&e, "cod ro12").as_deref(), Some("ro12"), "the value keeps the text's case");
}

#[test]
fn invalid_definitions_are_reported() {
    for kind in [ExtractKind::Regex("(".into()), ExtractKind::Regex("  ".into()), ExtractKind::Pattern(String::new()), ExtractKind::Words(vec![" ".into()])] {
        assert!(Extractor::new(&ex(kind.clone()), Lang::Ro).is_err(), "{kind:?}");
    }
    let mut e = ex(ExtractKind::Pattern("#".into()));
    e.occurrence = Occurrence::Nth(0);
    assert!(Extractor::new(&e, Lang::Ro).is_err());
}
