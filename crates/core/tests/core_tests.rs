use std::collections::HashMap;
use std::path::PathBuf;

use org_core::timefmt::{self, civil_from_days, days_from_civil, timestamp};
use org_core::*;

fn meta(path: &str, size: u64, created: i64, modified: i64) -> FileMeta {
    FileMeta::new(path, "/dl", size, created, modified)
}

fn text(field: TextField, op: TextOp, value: &str) -> Condition {
    Condition::Text { field, op, value: value.to_string() }
}

const NOW: i64 = 1_791_000_000;
fn ctx() -> EvalCtx<'static> {
    EvalCtx::new(NOW)
}

// ---------- date ----------

#[test]
fn civil_known_values() {
    let c = timefmt::civil(0, 0);
    assert_eq!((c.year, c.month, c.day, c.hour), (1970, 1, 1, 0));
    let c = timefmt::civil(1_700_000_000, 0);
    assert_eq!((c.year, c.month, c.day, c.hour, c.min, c.sec), (2023, 11, 14, 22, 13, 20));
    // 29 februarie 2024 (an bisect)
    let c = timefmt::civil(1_709_164_800, 0);
    assert_eq!((c.year, c.month, c.day), (2024, 2, 29));
    // 2026-10-10 00:00:00 UTC
    assert_eq!(timestamp(2026, 10, 10, 0, 0, 0), days_from_civil(2026, 10, 10) * 86_400);
}

#[test]
fn civil_round_trips_over_wide_range() {
    for days in (-800_000..800_000).step_by(37) {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days, "zi {days}");
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
    }
    // sfârșit de an și de lună
    assert_eq!(civil_from_days(days_from_civil(2025, 12, 31)), (2025, 12, 31));
    assert_eq!(civil_from_days(days_from_civil(2100, 2, 28) + 1), (2100, 3, 1)); // 2100 nu e bisect
}

#[test]
fn timezone_offset_moves_the_day() {
    let ts = timestamp(2026, 10, 9, 22, 30, 0);
    assert_eq!(timefmt::format(ts, 0, "AAAA-LL-ZZ", Lang::Ro), "2026-10-09");
    assert_eq!(timefmt::format(ts, 3 * 3600, "AAAA-LL-ZZ", Lang::Ro), "2026-10-10");
}

#[test]
fn format_tokens_and_month_names() {
    let ts = timestamp(2026, 3, 5, 7, 8, 9);
    assert_eq!(timefmt::format(ts, 0, "AAAA-LL-ZZ HH:MM:SS", Lang::Ro), "2026-03-05 07:08:09");
    assert_eq!(timefmt::format(ts, 0, "AA/LL", Lang::Ro), "26/03");
    assert_eq!(timefmt::format(ts, 0, "LLLL AAAA", Lang::Ro), "martie 2026");
    assert_eq!(timefmt::format(ts, 0, "LLLL AAAA", Lang::En), "March 2026");
    // caracterele care nu sunt jetoane rămân neschimbate, inclusiv cele cu diacritice
    assert_eq!(timefmt::format(ts, 0, "ăț-AAAA", Lang::Ro), "ăț-2026");
}

#[test]
fn english_date_tokens() {
    let ts = timestamp(2026, 3, 5, 7, 8, 9);
    assert_eq!(timefmt::format(ts, 0, "YYYY-MM-DD HH-mm-ss", Lang::En), "2026-03-05 07-08-09");
    assert_eq!(timefmt::format(ts, 0, "DD.MM.YY", Lang::En), "05.03.26");
    assert_eq!(timefmt::format(ts, 0, "MMMM YYYY", Lang::En), "March 2026");
    assert_eq!(timefmt::format(ts, 0, "MMMM", Lang::Ro), "martie", "the month name alone");
    // A Romanian format keeps MM as minutes, as in rules written before.
    assert_eq!(timefmt::format(ts, 0, "AAAA-LL-ZZ HH-MM", Lang::En), "2026-03-05 07-08");
    assert_eq!(timefmt::format(ts, 0, "MM", Lang::En), "03", "the month alone");
    assert_eq!(timefmt::format(ts, 0, "HH-mm", Lang::En), "07-08");
    assert!(timefmt::english_tokens("YYYY") && !timefmt::english_tokens("AAAA-LL-ZZ HH-MM"));
}

// ---------- condiții ----------

#[test]
fn text_conditions_are_case_insensitive_and_handle_diacritics() {
    let m = meta("/dl/Factură_Enel.PDF", 10, 0, 0);
    assert!(text(TextField::Extension, TextOp::Is, "pdf").matches_meta(&m, &ctx()));
    assert!(text(TextField::Extension, TextOp::Is, "PdF").matches_meta(&m, &ctx()));
    assert!(text(TextField::Name, TextOp::Contains, "FACTURĂ").matches_meta(&m, &ctx()));
    assert!(text(TextField::Name, TextOp::StartsWith, "fact").matches_meta(&m, &ctx()));
    assert!(text(TextField::Name, TextOp::EndsWith, "enel").matches_meta(&m, &ctx()));
    assert!(text(TextField::Name, TextOp::DoesNotContain, "digi").matches_meta(&m, &ctx()));
    assert!(text(TextField::Name, TextOp::IsNot, "altceva").matches_meta(&m, &ctx()));
    assert!(!text(TextField::Name, TextOp::Is, "factură").matches_meta(&m, &ctx()));
}

#[test]
fn is_nothing_matches_files_without_extension() {
    let m = meta("/dl/README", 1, 0, 0);
    assert!(text(TextField::Extension, TextOp::IsNothing, "").matches_meta(&m, &ctx()));
    let m2 = meta("/dl/a.txt", 1, 0, 0);
    assert!(!text(TextField::Extension, TextOp::IsNothing, "").matches_meta(&m2, &ctx()));
}

#[test]
fn double_extension_uses_the_last_part() {
    let m = meta("/dl/backup.tar.gz", 1, 0, 0);
    assert!(text(TextField::Extension, TextOp::Is, "gz").matches_meta(&m, &ctx()));
    assert_eq!(m.stem(), "backup.tar");
}

#[test]
fn folder_path_conditions_exclude_subfolders() {
    let m = meta("/dl/temp/x.pdf", 1, 0, 0);
    assert!(text(TextField::Folder, TextOp::Contains, "temp").matches_meta(&m, &ctx()));
    assert!(text(TextField::Path, TextOp::Contains, "temp").matches_meta(&m, &ctx()));
    assert!(text(TextField::Folder, TextOp::DoesNotContain, "arhiva").matches_meta(&m, &ctx()));
}

#[test]
fn size_conditions() {
    let m = meta("/dl/a.bin", 1024, 0, 0);
    assert!(Condition::Size { op: SizeOp::Larger, bytes: 1023 }.matches_meta(&m, &ctx()));
    assert!(!Condition::Size { op: SizeOp::Larger, bytes: 1024 }.matches_meta(&m, &ctx()));
    assert!(Condition::Size { op: SizeOp::Smaller, bytes: 1025 }.matches_meta(&m, &ctx()));
    assert!(!Condition::Size { op: SizeOp::Smaller, bytes: 1024 }.matches_meta(&m, &ctx()));
}

#[test]
fn date_conditions() {
    let day = 86_400;
    let m = meta("/dl/a.bin", 1, NOW - 10 * day, NOW - 2 * day);
    let c = |field, op| Condition::Date { field, op };
    assert!(c(DateField::Created, DateOp::OlderThanDays(7)).matches_meta(&m, &ctx()));
    assert!(!c(DateField::Created, DateOp::OlderThanDays(30)).matches_meta(&m, &ctx()));
    assert!(c(DateField::Modified, DateOp::WithinDays(3)).matches_meta(&m, &ctx()));
    assert!(!c(DateField::Modified, DateOp::WithinDays(1)).matches_meta(&m, &ctx()));
    assert!(c(DateField::Modified, DateOp::After(NOW - 3 * day)).matches_meta(&m, &ctx()));
    assert!(c(DateField::Created, DateOp::Before(NOW - 9 * day)).matches_meta(&m, &ctx()));
}

fn invoice_condition() -> Condition {
    Condition::All(vec![
        text(TextField::Extension, TextOp::Is, "pdf"),
        Condition::Any(vec![
            text(TextField::Name, TextOp::Contains, "factura"),
            text(TextField::Name, TextOp::Contains, "invoice"),
        ]),
        text(TextField::Folder, TextOp::DoesNotContain, "temp"),
        Condition::Size { op: SizeOp::Smaller, bytes: 20 * 1024 * 1024 },
    ])
}

#[test]
fn nested_groups_follow_boolean_logic() {
    let c = invoice_condition();
    assert!(c.matches_meta(&meta("/dl/Factura_Enel.pdf", 1000, 0, 0), &ctx()));
    assert!(c.matches_meta(&meta("/dl/invoice-44.pdf", 1000, 0, 0), &ctx()));
    assert!(!c.matches_meta(&meta("/dl/poza.pdf", 1000, 0, 0), &ctx()));
    assert!(!c.matches_meta(&meta("/dl/Factura.docx", 1000, 0, 0), &ctx()));
    assert!(!c.matches_meta(&meta("/dl/temp/Factura.pdf", 1000, 0, 0), &ctx()));
    assert!(!c.matches_meta(&meta("/dl/Factura.pdf", 30 * 1024 * 1024, 0, 0), &ctx()));
}

#[test]
fn optimization_reorders_by_cost_without_changing_results() {
    let plain = invoice_condition();
    let fast = invoice_condition().optimized();
    // Dimensiunea (cost 3) trece la urmă, extensia (cost 1) rămâne prima.
    if let Condition::All(children) = &fast {
        assert!(matches!(children.last(), Some(Condition::Size { .. })));
        assert!(matches!(children.first(), Some(Condition::Text { field: TextField::Extension, .. })));
    } else {
        panic!("rădăcina trebuie să rămână un grup");
    }
    let samples = [
        meta("/dl/Factura_Enel.pdf", 1000, 0, 0),
        meta("/dl/invoice-44.pdf", 30 * 1024 * 1024, 0, 0),
        meta("/dl/temp/Factura.pdf", 10, 0, 0),
        meta("/dl/notite.txt", 10, 0, 0),
        meta("/dl/FACTURA.PDF", 10, 0, 0),
    ];
    for s in &samples {
        assert_eq!(plain.matches_meta(s, &ctx()), fast.matches_meta(s, &ctx()), "{:?}", s.path);
    }
}

#[test]
fn validation_rules() {
    assert_eq!(Condition::All(vec![]).validate(), Err(RuleError::EmptyGroup));
    assert_eq!(text(TextField::Name, TextOp::Is, "x").validate(), Err(RuleError::TopMustBeGroup));
    assert_eq!(Condition::AllFiles.validate(), Ok(()));
    assert_eq!(
        Condition::All(vec![Condition::AllFiles]).validate(),
        Err(RuleError::AllFilesNotTop)
    );
    // 8 niveluri sunt permise, 9 nu
    let mut c = text(TextField::Name, TextOp::Is, "x");
    for _ in 0..MAX_DEPTH {
        c = Condition::All(vec![c]);
    }
    assert_eq!(c.validate(), Ok(()));
    assert_eq!(Condition::Any(vec![c]).validate(), Err(RuleError::TooDeep));
}

// ---------- șabloane ----------

fn render(tpl: &str, m: &FileMeta, mode: Mode) -> Result<String, TemplateError> {
    let dicts: HashMap<String, Dictionary> = HashMap::from([(
        "Furnizor".to_string(),
        Dictionary {
            field: TextField::Name,
            entries: vec![("enel".into(), "Energie".into()), ("digi".into(), "Internet".into())],
            default: Some("Altele".into()),
            extract: None,
        },
    )]);
    let ctx = RenderCtx { now: NOW, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &dicts, content: None, raw: None };
    Template::parse(tpl)?.render(m, &ctx, mode)
}

#[test]
fn template_variables() {
    let created = timestamp(2026, 10, 8, 12, 0, 0);
    let m = meta("/dl/Factura Enel.pdf", 2048, created, created);
    assert_eq!(render("{created:AAAA}/{created:LL}", &m, Mode::Path).unwrap(), "2026/10");
    assert_eq!(render("{created:AAAA-LL-ZZ}_{name}", &m, Mode::Path).unwrap(), "2026-10-08_Factura Enel");
    assert_eq!(render("{ext}", &m, Mode::Path).unwrap(), "pdf");
    assert_eq!(render("{file}", &m, Mode::Path).unwrap(), "Factura Enel.pdf");
    assert_eq!(render("{size}", &m, Mode::Path).unwrap(), "2048");
    assert_eq!(render("{now:AAAA}", &m, Mode::Path).unwrap(), "2026");
}

#[test]
fn template_literals_and_escapes() {
    let m = meta("/dl/a.pdf", 1, 0, 0);
    assert_eq!(render("{{{name}}}", &m, Mode::Path).unwrap(), "{a}");
    assert_eq!(render("D:\\Facturi\\{name}", &m, Mode::Path).unwrap(), "D:\\Facturi\\a");
}

#[test]
fn template_parse_errors() {
    assert_eq!(Template::parse("{name").unwrap_err(), TemplateError::UnclosedBrace);
    assert_eq!(Template::parse("name}").unwrap_err(), TemplateError::UnmatchedBrace);
    assert!(matches!(Template::parse("{nimic}"), Err(TemplateError::UnknownVariable(_))));
    assert!(matches!(Template::parse("{created}"), Err(TemplateError::UnknownVariable(_))));
    assert!(matches!(Template::parse("{var:}"), Err(TemplateError::UnknownVariable(_))));
}

#[test]
fn dictionary_variables() {
    let m = meta("/dl/Factura_Enel_09.pdf", 1, 0, 0);
    assert_eq!(render("{var:Furnizor}", &m, Mode::Path).unwrap(), "Energie");
    let m2 = meta("/dl/Factura_Orange.pdf", 1, 0, 0);
    assert_eq!(render("{var:Furnizor}", &m2, Mode::Path).unwrap(), "Altele");
    assert_eq!(render("{var:Lipsa}", &m, Mode::Path).unwrap_err(), TemplateError::UnknownNamed("Lipsa".into()));
}

#[test]
fn path_mode_neutralizes_hostile_names() {
    // O valoare nu poate ieși din folderul de destinație.
    assert_eq!(org_core::template::sanitize(".."), "_");
    assert_eq!(org_core::template::sanitize("."), "_");
    assert_eq!(org_core::template::sanitize("a/b\\c"), "a_b_c");
    assert_eq!(org_core::template::sanitize("..a"), "..a");
    let m = meta("/dl/a:b*c?.pdf", 1, 0, 0);
    assert_eq!(render("{file}", &m, Mode::Path).unwrap(), "a_b_c_.pdf");
    // În modul Raw valorile rămân neschimbate (argumente de program, fără shell).
    assert_eq!(render("{file}", &m, Mode::Raw).unwrap(), "a:b*c?.pdf");
}

// ---------- reguli ----------

fn base_rule() -> Rule {
    Rule {
        id: 1,
        name: "Facturi".into(),
        enabled: true,
        hidden: false,
        folders: vec![WatchedFolder { path: PathBuf::from("/dl"), recursive: true }],
        condition: invoice_condition(),
        actions: vec![Action::Move {
            dest: Template::literal("/arhiva"),
            conflict: Conflict::Rename,
            keep_structure: false,
            remove_empty: false,
        }],
        continue_after_move: false, simulate: false,
    }
}

#[test]
fn rule_validation() {
    assert_eq!(base_rule().validate(), Ok(()));
    let mut r = base_rule();
    r.folders.clear();
    assert_eq!(r.validate(), Err(RuleError::NoFolders));
    let mut r = base_rule();
    r.actions.clear();
    assert_eq!(r.validate(), Err(RuleError::NoActions));
    let mut r = base_rule();
    r.actions = vec![Action::Trash { remove_empty: false }, Action::Rename {
        name: Template::literal("x"),
        conflict: Conflict::Rename,
    }];
    assert_eq!(r.validate(), Err(RuleError::ActionAfterRemoval));
    // Trash ca ultimă acțiune e în regulă.
    let mut r = base_rule();
    r.actions = vec![Action::Trash { remove_empty: true }];
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn watched_folders_respect_the_recursive_flag() {
    let mut r = base_rule();
    r.folders = vec![
        WatchedFolder { path: PathBuf::from("/dl"), recursive: false },
        WatchedFolder { path: PathBuf::from("/scan"), recursive: true },
    ];
    assert!(r.watching(&PathBuf::from("/dl/a.pdf")).is_some());
    assert!(r.watching(&PathBuf::from("/dl/sub/a.pdf")).is_none());
    assert!(r.watching(&PathBuf::from("/scan/2026/a.pdf")).is_some());
    assert!(r.watching(&PathBuf::from("/altceva/a.pdf")).is_none());
    // „/dl2” nu e sub „/dl”
    assert!(r.watching(&PathBuf::from("/dl2/a.pdf")).is_none());
}

#[test]
fn disabled_rules_never_apply() {
    let mut r = base_rule();
    let m = meta("/dl/Factura.pdf", 10, 0, 0);
    assert!(r.applies_to(&m, &ctx()));
    r.enabled = false;
    assert!(!r.applies_to(&m, &ctx()));
}

#[test]
fn hidden_rules_are_invisible_until_unlocked() {
    let mut hidden = base_rule();
    hidden.id = 2;
    hidden.hidden = true;
    let rules = vec![base_rule(), hidden];
    let locked: Vec<u64> = visible_rules(&rules, false).map(|r| r.id).collect();
    let open: Vec<u64> = visible_rules(&rules, true).map(|r| r.id).collect();
    assert_eq!(locked, vec![1]);
    assert_eq!(open, vec![1, 2]);
}

#[test]
fn relative_parent_keeps_subfolder_structure() {
    let m = FileMeta::new("/dl/a/b/x.pdf", "/dl", 1, 0, 0);
    assert_eq!(m.relative_parent(), Some(PathBuf::from("a/b")));
    let m = FileMeta::new("/dl/x.pdf", "/dl", 1, 0, 0);
    assert_eq!(m.relative_parent(), Some(PathBuf::new()));
    let m = FileMeta::new("/altundeva/x.pdf", "/dl", 1, 0, 0);
    assert_eq!(m.relative_parent(), None);
}

/// Ora de vară se aplică după data fișierului, nu după data de azi.
#[test]
fn dates_use_the_offset_valid_at_that_moment() {
    fn bucuresti(ts: i64) -> i64 {
        // simplificat: aprilie–septembrie UTC+3, altfel UTC+2
        let m = org_core::timefmt::civil(ts, 0).month;
        if (4..=9).contains(&m) { 3 * 3600 } else { 2 * 3600 }
    }
    let dicts = std::collections::HashMap::new();
    let ctx = RenderCtx { now: timestamp(2026, 12, 1, 12, 0, 0), tz: Tz::At(bucuresti), lang: Lang::Ro, dicts: &dicts, content: None, raw: None };
    // creat pe 15 iulie la 21:30 UTC = 16 iulie 00:30 ora României (vară)
    let m = FileMeta::new("/a/x.pdf", "/a", 1, timestamp(2026, 7, 15, 21, 30, 0), 0);
    let t = Template::parse("{created:AAAA-LL-ZZ HH:MM}").unwrap();
    assert_eq!(t.render(&m, &ctx, Mode::Raw).unwrap(), "2026-07-16 00:30");
}

/// O intrare de dicționar poate avea mai multe chei separate prin virgulă.
#[test]
fn dictionary_key_with_commas_matches_any_of_them() {
    let mut dicts = std::collections::HashMap::new();
    dicts.insert(
        "Tip".to_string(),
        Dictionary {
            field: TextField::Extension,
            entries: vec![("pdf, doc,docx , ,".into(), "Documente".into()), ("jpg,png".into(), "Imagini".into())],
            default: Some("Altele".into()),
            extract: None,
        },
    );
    let ctx = RenderCtx { now: 0, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &dicts, content: None, raw: None };
    let t = Template::parse("{var:Tip}").unwrap();
    let r = |f: &str| t.render(&FileMeta::new(f, "/a", 1, 0, 0), &ctx, Mode::Raw).unwrap();
    assert_eq!(r("/a/x.PDF"), "Documente");
    assert_eq!(r("/a/x.docx"), "Documente");
    assert_eq!(r("/a/x.png"), "Imagini");
    assert_eq!(r("/a/x.zip"), "Altele", "virgulele și spațiile goale nu potrivesc orice");
}

#[test]
fn windows_unsafe_names_are_fixed() {
    assert_eq!(safe_name("CON"), "_CON");
    assert_eq!(safe_name("con.txt"), "_con.txt");
    assert_eq!(safe_name("Com1.pdf"), "_Com1.pdf");
    assert_eq!(safe_name("LPT9"), "_LPT9");
    assert_eq!(safe_name("COM0"), "COM0");
    assert_eq!(safe_name("CONSOLE.txt"), "CONSOLE.txt");
    assert_eq!(safe_name("Doc. "), "Doc");
    assert_eq!(safe_name("..."), "_");
    assert_eq!(safe_name(""), "_");
    let long = format!("{}.pdf", "a".repeat(300));
    let s = safe_name(&long);
    assert_eq!(s.chars().count(), 255);
    assert!(s.ends_with(".pdf"));
}

#[test]
fn folder_keeps_structure_in_destinations() {
    let m = FileMeta::new("C:\\Users\\o\\Downloads\\a.pdf", "C:\\Users\\o\\Downloads", 1, 0, 0);
    let sep = std::path::MAIN_SEPARATOR_STR;
    let folder = m.folder();
    // La început: calea absolută, neschimbată.
    assert_eq!(render("{folder}\\PDF", &m, Mode::Dir).unwrap(), format!("{folder}\\PDF"));
    // În mijloc: ramură relativă, fără „:”.
    let mid = render("D:\\Copie\\{folder}", &m, Mode::Dir).unwrap();
    assert!(!mid["D:\\Copie\\".len()..].contains(':'), "{mid}");
    assert_eq!(relative_tree("C:\\Users\\o"), ["C", "Users", "o"].join(sep));
    assert_eq!(relative_tree("\\\\srv\\share\\x"), ["srv", "share", "x"].join(sep));
    assert_eq!(relative_tree("C:\\a\\..\\b"), ["C", "a", "_", "b"].join(sep));
    // În nume (Mode::Path) rămâne o singură componentă.
    assert!(!render("{folder}", &m, Mode::Path).unwrap().contains(['\\', '/']));
}

#[test]
fn shell_mode_quotes_values_outside_quotes() {
    let m = meta("/dl/a & b.pdf", 1, 0, 0);
    assert_eq!(render("echo {file} > out.txt", &m, Mode::Shell).unwrap(), "echo \"a & b.pdf\" > out.txt");
    assert_eq!(render("copy \"{file}\" x", &m, Mode::Shell).unwrap(), "copy \"a & b.pdf\" x");
    assert_eq!(render("\"{name}\" \"{ext}\" {ext}", &m, Mode::Shell).unwrap(), "\"a & b\" \"pdf\" \"pdf\"");
}

#[test]
fn sha256_hmac_pbkdf2_match_known_vectors() {
    use org_core::secret::*;
    assert_eq!(to_hex(&sha256(&[b""])), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(to_hex(&sha256(&[b"ab", b"c"])), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let long = "a".repeat(1000);
    assert_eq!(to_hex(&sha256(&[long.as_bytes()])), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    // RFC 4231, cazul 2.
    assert_eq!(
        to_hex(&hmac_sha256(b"Jefe", &[b"what do ya want for nothing?"])),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // RFC 7914 §11 (PBKDF2-HMAC-SHA256), primii 32 de octeți.
    assert_eq!(
        to_hex(&pbkdf2_sha256(b"passwd", b"salt", 1)),
        "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
    );
    assert_eq!(
        to_hex(&pbkdf2_sha256(b"password", b"salt", 4096)),
        "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
    );
    let ph = PasswordHash::new("parolă", &new_salt());
    assert!(ph.verify("parolă"));
    assert!(!ph.verify("parola"));
    assert_ne!(new_salt(), new_salt());
}

#[test]
fn content_verdicts_are_reused_and_unreadable_files_are_not_remembered() {
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;
    struct L {
        text: Option<&'static str>,
        loads: Cell<usize>,
        memo: RefCell<Vec<(u64, bool)>>,
    }
    impl ContentLoader for L {
        fn load(&self, _m: &FileMeta) -> Option<Arc<str>> {
            self.loads.set(self.loads.get() + 1);
            self.text.map(Arc::from)
        }
        fn verdict(&self, _m: &FileMeta, key: u64) -> Option<bool> {
            self.memo.borrow().iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
        }
        fn remember(&self, _m: &FileMeta, key: u64, v: bool) {
            self.memo.borrow_mut().push((key, v));
        }
    }
    let m = FileMeta::new("C:/dl/factura.pdf", "C:/dl", 10, 0, 0);
    let c = Condition::Text { field: TextField::Content, op: TextOp::Contains, value: "Factura".into() };
    let ctx = EvalCtx::new(0);
    let l = L { text: Some("factura nr. 12"), loads: Cell::new(0), memo: RefCell::new(vec![]) };
    assert!(c.matches(&Prepared::with_content(&m, &l), &ctx));
    assert_eq!(l.loads.get(), 1);
    // A doua evaluare (altă trecere): din memorie, fără citire.
    assert!(c.matches(&Prepared::with_content(&m, &l), &ctx));
    assert_eq!(l.loads.get(), 1);
    // Cheia nu depinde de majuscule, dar de operație și valoare.
    assert_eq!(content_key(TextOp::Contains, "Factura"), content_key(TextOp::Contains, "factura"));
    assert_ne!(content_key(TextOp::Contains, "factura"), content_key(TextOp::DoesNotContain, "factura"));
    assert_ne!(content_key(TextOp::Contains, "factura"), content_key(TextOp::Contains, "facturi"));
    // Fără text citit: nu se ține minte.
    let l = L { text: None, loads: Cell::new(0), memo: RefCell::new(vec![]) };
    assert!(!c.matches(&Prepared::with_content(&m, &l), &ctx));
    assert!(l.memo.borrow().is_empty());
}

#[test]
fn extract_variables_render_from_the_name_and_the_text() {
    use std::sync::Arc;
    let t = |s: &str| Template::parse(s).unwrap();
    let mut dicts = HashMap::new();
    dicts.insert(
        "Code".to_string(),
        Dictionary {
            field: TextField::Name,
            entries: Vec::new(),
            default: Some("none".into()),
            extract: Some(Extract { kind: ExtractKind::Pattern("@@-###".into()), ..Extract::default() }),
        },
    );
    dicts.insert(
        "Total".to_string(),
        Dictionary {
            field: TextField::Content,
            entries: Vec::new(),
            default: None,
            extract: Some(Extract { kind: ExtractKind::Pattern("%".into()), before: "Total:".into(), ..Extract::default() }),
        },
    );
    dicts.insert(
        "Bad".to_string(),
        Dictionary { field: TextField::Name, entries: Vec::new(), default: None, extract: Some(Extract { kind: ExtractKind::Regex("(".into()), ..Extract::default() }) },
    );
    let m = FileMeta::new("/dl/Order AB-123 final.pdf", "/dl", 1, NOW, NOW);
    let mut ctx = RenderCtx { now: NOW, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &dicts, content: None, raw: None };
    assert_eq!(t("{var:Code}").render(&m, &ctx, Mode::Path).unwrap(), "AB-123", "the original case of the name");
    assert_eq!(t("{var:Total}").render(&m, &ctx, Mode::Path), Err(TemplateError::MissingValue("Total".into())));
    ctx.raw = Some(Arc::from("Subtotal 10\nTOTAL: 1.234,56 RON"));
    assert_eq!(t("{var:Total}").render(&m, &ctx, Mode::Path).unwrap(), "1.234,56");
    assert!(matches!(t("{var:Bad}").render(&m, &ctx, Mode::Path), Err(TemplateError::InvalidVariable(..))));
    let other = FileMeta::new("/dl/notes.pdf", "/dl", 1, NOW, NOW);
    assert_eq!(t("{var:Code}").render(&other, &ctx, Mode::Path).unwrap(), "none", "the default value");
}

#[test]
fn variable_conditions() {
    let mut dicts = HashMap::new();
    dicts.insert(
        "Year".to_string(),
        Dictionary {
            field: TextField::Name,
            entries: Vec::new(),
            default: Some("unused".into()),
            extract: Some(Extract { kind: ExtractKind::Regex(r"\b(20\d\d)\b".into()), ..Extract::default() }),
        },
    );
    dicts.insert(
        "Kind".to_string(),
        Dictionary { field: TextField::Extension, entries: vec![("pdf, docx".into(), "Docs".into())], default: Some("Other".into()), extract: None },
    );
    let ctx = EvalCtx { now: NOW, vars: Some(&dicts) };
    let found = |name: &str, file: &str| Condition::Var { name: name.into(), found: true }.matches_meta(&FileMeta::new(file, "/", 1, NOW, NOW), &ctx);
    assert!(found("Year", "/r/report 2026.pdf"));
    assert!(!found("Year", "/r/report.pdf"), "the default value does not count as found");
    assert!(found("Kind", "/r/a.DOCX"));
    assert!(!found("Kind", "/r/a.jpg"));
    assert!(!found("Unknown", "/r/a.jpg"));
    let not = Condition::Var { name: "Year".into(), found: false };
    assert!(not.matches_meta(&FileMeta::new("/r/x.pdf", "/", 1, NOW, NOW), &ctx));
}
