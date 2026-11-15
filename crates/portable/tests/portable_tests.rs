use std::collections::BTreeMap;
use std::path::PathBuf;

use org_core::*;
use org_portable::json::{self, Value};
use org_portable::*;

fn t(s: &str) -> Template {
    Template::parse(s).unwrap()
}

fn sample_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: 7,
            name: "Facturi „2026”".into(),
            enabled: true,
            hidden: false,
            folders: vec![
                WatchedFolder { path: PathBuf::from("C:\\Users\\me\\Downloads"), recursive: true },
                WatchedFolder { path: PathBuf::from("/mnt/x"), recursive: false },
            ],
            condition: Condition::All(vec![
                Condition::Any(vec![
                    Condition::Text { field: TextField::Extension, op: TextOp::Is, value: "pdf".into() },
                    Condition::Text { field: TextField::Name, op: TextOp::Contains, value: "factur\"ă".into() },
                ]),
                Condition::Size { op: SizeOp::Larger, bytes: 1024 },
                Condition::Date { field: DateField::Modified, op: DateOp::OlderThanDays(30) },
                Condition::Date { field: DateField::Created, op: DateOp::Before(1_700_000_000) },
            ]),
            actions: vec![
                Action::Copy { dest: t("D:\\bk"), conflict: Conflict::Overwrite, keep_structure: true, then_target: false },
                Action::Move {
                    dest: t("D:\\Facturi\\{created:AAAA}\\{var:Furnizor}"),
                    conflict: Conflict::Skip,
                    keep_structure: false,
                    remove_empty: true,
                },
                Action::Rename { name: t("{created:AAAA-LL-ZZ}_{name} {{x}}"), conflict: Conflict::Rename },
                Action::ChangeExtension { ext: t("bak") },
                Action::Run { program: t("tool.exe"), args: vec![t("--in"), t("{path}")] },
                Action::Command { line: t("echo {name} >> \"D:\\log.txt\"") },
            ],
            continue_after_move: false, simulate: false,
        },
        Rule {
            id: 8,
            name: "Curățenie".into(),
            enabled: false,
            hidden: true,
            folders: vec![WatchedFolder { path: PathBuf::from("/tmp"), recursive: true }],
            condition: Condition::AllFiles,
            actions: vec![Action::Trash { remove_empty: true }],
            continue_after_move: true, simulate: false,
        },
    ]
}

fn sample_dicts() -> BTreeMap<String, Dictionary> {
    let mut m = BTreeMap::new();
    m.insert(
        "Furnizor".into(),
        Dictionary {
            field: TextField::Name,
            entries: vec![("enel".into(), "Enel".into()), ("digi".into(), "Digi".into())],
            default: Some("Altele".into()),
        },
    );
    m
}

fn zero_ids(mut r: Vec<Rule>) -> Vec<Rule> {
    r.iter_mut().for_each(|x| x.id = 0);
    r
}

#[test]
fn round_trip_including_hidden() {
    let text = export(&sample_rules(), &sample_dicts(), true);
    let b = import(&text).unwrap();
    // Id-urile (permanente) se exportă și se citesc înapoi.
    assert_eq!(b.rules, sample_rules());
    assert_eq!(b.dictionaries, sample_dicts());
    // a doua serializare e identică: format stabil
    assert_eq!(export(&b.rules, &b.dictionaries, true), text);
}

#[test]
fn hidden_rules_are_not_exported_by_default() {
    let text = export(&sample_rules(), &sample_dicts(), false);
    assert!(!text.contains("Curățenie"));
    assert_eq!(import(&text).unwrap().rules.len(), 1);
}

#[test]
fn newer_version_and_foreign_files_are_refused() {
    assert_eq!(
        import(r#"{"app":"organizator","schema_version":99,"rules":[]}"#),
        Err(ImportError::NewerVersion(99))
    );
    assert!(matches!(import(r#"{"app":"altceva","schema_version":1,"rules":[]}"#), Err(ImportError::Invalid(..))));
    assert!(matches!(import("nu e json"), Err(ImportError::Json(_))));
}

#[test]
fn invalid_rules_name_the_culprit() {
    let bad_action = r#"{"app":"organizator","schema_version":1,"rules":[
      {"name":"R1","folders":[{"path":"/a"}],"condition":{"type":"all_files"},
       "actions":[{"type":"move","dest":"/b/{nu_exista}"}]}]}"#;
    let e = import(bad_action).unwrap_err().to_string();
    assert!(e.contains("R1") && e.contains("șablon"), "{e}");

    let no_actions = r#"{"app":"organizator","schema_version":1,"rules":[
      {"name":"R2","folders":[{"path":"/a"}],"condition":{"type":"all_files"},"actions":[]}]}"#;
    assert!(import(no_actions).unwrap_err().to_string().contains("NoActions"));

    let after_delete = r#"{"app":"organizator","schema_version":1,"rules":[
      {"name":"R3","folders":[{"path":"/a"}],"condition":{"type":"all_files"},
       "actions":[{"type":"delete"},{"type":"trash"}]}]}"#;
    assert!(import(after_delete).unwrap_err().to_string().contains("ActionAfterRemoval"));
}

#[test]
fn defaults_for_missing_optional_fields() {
    let min = r#"{"app":"organizator","schema_version":1,"rules":[
      {"name":"R","folders":[{"path":"/a"}],"condition":{"type":"all_files"},
       "actions":[{"type":"move","dest":"/b"}]}]}"#;
    let r = &import(min).unwrap().rules[0];
    assert!(r.enabled && !r.hidden && r.continue_after_move && r.folders[0].recursive);
    assert!(matches!(&r.actions[0], Action::Move { conflict: Conflict::Rename, .. }));
}

#[test]
fn json_strings_escapes_and_unicode() {
    let v = json::parse(r#""a\"b\\c\n\u00e9\ud83d\ude00""#).unwrap();
    assert_eq!(v, Value::Str("a\"b\\c\né😀".into()));
    let out = json::to_string_pretty(&v);
    assert_eq!(json::parse(&out).unwrap(), v);
}

#[test]
fn json_rejects_garbage_without_panicking() {
    for bad in [
        "", "{", "[1,]", r#"{"a" 1}"#, "1.5", "01x", r#""\ud800""#, r#""\x""#, "\"\u{1}\"", "nul", "[1] 2",
        r#""abc"#,
    ] {
        assert!(json::parse(bad).is_err(), "{bad:?}");
    }
    let deep = "[".repeat(10_000);
    assert!(json::parse(&deep).is_err());
    let deep_cond = format!(
        r#"{{"app":"organizator","schema_version":1,"rules":[{{"name":"d","folders":[{{"path":"/a"}}],"condition":{},"actions":[{{"type":"trash"}}]}}]}}"#,
        "{\"type\":\"all\",\"items\":[".repeat(40) + "{\"type\":\"all_files\"}" + &"]}".repeat(40)
    );
    assert!(import(&deep_cond).is_err());
}

#[test]
fn template_source_round_trips() {
    for s in ["a{{b}}c", "{name}.{ext}", "{created:AAAA-LL-ZZ}", "{var:X}", "plain", "{{}}"] {
        assert_eq!(Template::parse(s).unwrap().to_source(), s);
    }
}

#[test]
fn stable_ids_depend_on_the_name_and_disambiguate_duplicates() {
    let mk = |n: &str| Rule {
        id: 0,
        name: n.into(),
        enabled: true,
        hidden: false,
        folders: vec![],
        condition: Condition::AllFiles,
        actions: vec![],
        continue_after_move: true, simulate: false,
    };
    let mut a = vec![mk("A"), mk("B"), mk("A")];
    assign_ids(&mut a);
    let mut b = vec![mk("B"), mk("A")];
    assign_ids(&mut b);
    assert_eq!(a[1].id, b[0].id, "id-ul nu depinde de poziție");
    assert_eq!(a[0].id, b[1].id);
    assert_ne!(a[0].id, a[2].id, "duplicatele se deosebesc");
    assert!(a.iter().all(|r| r.id <= i64::MAX as u64));
}

#[test]
fn unfinished_disabled_rules_load_but_enabled_ones_must_be_valid() {
    let doc = |enabled: bool| {
        format!(
            r#"{{"app":"organizator","schema_version":1,"rules":[
          {{"name":"R","enabled":{enabled},"folders":[],"condition":{{"type":"all","items":[]}},"actions":[]}}]}}"#
        )
    };
    assert_eq!(import(&doc(false)).unwrap().rules.len(), 1);
    assert!(import(&doc(true)).is_err());
}

#[test]
fn ids_survive_renaming_and_duplicates_get_new_ones() {
    let mut rules = zero_ids(sample_rules());
    assign_ids(&mut rules);
    let first = rules[0].id;
    assert_eq!(first, stable_id(&rules[0].name, &Default::default()), "fără id: același ca înainte (din nume)");
    // salvat, redenumit, reîncărcat: id-ul rămâne
    let mut b = import(&export(&rules, &Default::default(), true)).unwrap();
    b.rules[0].name = "Alt nume".into();
    let mut again = import(&export(&b.rules, &Default::default(), true)).unwrap().rules;
    assign_ids(&mut again);
    assert_eq!(again[0].id, first);
    // o copie cu același id (Duplică, import) primește alt id; originalul îl păstrează
    let mut dup = again.clone();
    let mut copy = dup[0].clone();
    copy.name = "Copie".into();
    dup.push(copy);
    assign_ids(&mut dup);
    assert_eq!(dup[0].id, first);
    assert_ne!(dup.last().unwrap().id, first);
    let ids: std::collections::HashSet<u64> = dup.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), dup.len());
}

#[test]
fn id_is_text_and_large_values_are_exact() {
    let t = r#"{"app":"organizator","schema_version":1,"rules":[{"id":"9223372036854775807","name":"x","enabled":false,
        "folders":[],"condition":{"type":"all_files"},"actions":[]}]}"#;
    assert_eq!(import(t).unwrap().rules[0].id, i64::MAX as u64);
    let bad_id = t.replace("9223372036854775807", "abc");
    assert!(import(&bad_id).is_err());
    let zero = t.replace("\"9223372036854775807\"", "\"0\"");
    assert!(import(&zero).is_err());
}

#[test]
fn requests_roundtrip_and_reject_garbage() {
    use org_portable::request::*;
    use std::path::PathBuf;
    for r in [
        Request::Pending { rule: u64::MAX >> 1, paths: vec![PathBuf::from("C:\\dl\\a b.pdf"), PathBuf::from("/x/ș.txt")] },
        Request::PendingRule { rule: 42 },
    ] {
        assert_eq!(parse_request(&request_to_json(&r)).unwrap(), r);
    }
    assert!(parse_request("{nu").is_err());
    assert!(parse_request(r#"{"kind":"pending","rule":"x","paths":[]}"#).is_err());
    assert!(parse_request(r#"{"kind":"pending","rule":"5","paths":[""]}"#).is_err());
    assert!(parse_request(r#"{"kind":"sterge_tot","rule":"5"}"#).is_err());
}

#[test]
fn hidden_rules_come_back_in_place_when_saving_locked() {
    let mk = |id: u64, name: &str, hidden: bool| Rule {
        id,
        name: name.into(),
        enabled: true,
        hidden,
        folders: vec![],
        condition: Condition::AllFiles,
        actions: vec![],
        continue_after_move: true, simulate: false,
    };
    let disk = vec![mk(1, "a", false), mk(2, "secret", true), mk(3, "b", false), mk(4, "x", true)];
    // Fereastra blocată a văzut doar a, b; a redenumit b, a adăugat c, și (dintr-un JSON lipit) d cu id-ul 2.
    let visible = vec![mk(1, "a", false), mk(3, "b2", false), mk(0, "c", false), mk(2, "d", false)];
    let mut out = merge_hidden(visible, &disk);
    assign_ids(&mut out);
    let names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["a", "secret", "b2", "x", "c", "d"]);
    assert_eq!(out[1].id, 2, "regula ascunsă își păstrează id-ul");
    assert_ne!(out[5].id, 2);
    let ids: std::collections::HashSet<u64> = out.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 6);
    // Fără reguli pe disc: rămân cele din fereastră.
    assert_eq!(merge_hidden(vec![mk(5, "n", false)], &[]).len(), 1);
}
