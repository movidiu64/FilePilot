use std::collections::HashMap;
use std::path::PathBuf;

use org_actions::*;
use org_core::timefmt::{timestamp, Lang};
use org_core::*;
use org_fs::MemFs;
use org_journal::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn t(s: &str) -> Template {
    Template::parse(s).unwrap()
}

fn rule(actions: Vec<Action>) -> Rule {
    Rule {
        id: 1,
        name: "t".into(),
        enabled: true,
        hidden: false,
        folders: vec![WatchedFolder { path: p("/dl"), recursive: true }],
        condition: Condition::AllFiles,
        actions,
        continue_after_move: true, simulate: false,
    }
}

fn mv(dest: &str) -> Action {
    Action::Move { dest: t(dest), conflict: Conflict::Rename, keep_structure: false, remove_empty: false }
}

fn ctx(d: &HashMap<String, Dictionary>) -> RenderCtx<'_> {
    RenderCtx { now: timestamp(2026, 10, 10, 12, 0, 0), tz: Tz::Fixed(0), lang: Lang::Ro, dicts: d, content: None, raw: None }
}

fn meta(path: &str) -> FileMeta {
    FileMeta::new(path, "/dl", 100, timestamp(2025, 3, 7, 9, 0, 0), timestamp(2025, 3, 7, 9, 0, 0))
}

fn invoice_rule() -> Rule {
    rule(vec![
        mv("/arhiva/{created:AAAA}/{created:LL}"),
        Action::Rename { name: t("{created:AAAA-LL-ZZ}_{name}"), conflict: Conflict::Rename },
    ])
}

#[test]
fn invoice_scenario() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/factura.pdf", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let r = run_rule(&mut fs, &mut j, &invoice_rule(), &meta("/dl/factura.pdf"), &ctx(&d));
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(r.path, p("/arhiva/2025/03/2025-03-07_factura.pdf"));
    assert_eq!(fs.file_paths(), vec![r.path.clone()]);
    assert_eq!(j.by_run(r.run_id).len(), 2);
    assert!(j.entries().iter().all(|e| e.status == Status::Done));
}

#[test]
fn conflict_policies() {
    let d = HashMap::new();
    for (c, expect) in [
        (Conflict::Rename, vec!["/b/x.pdf", "/b/x_2.pdf"]),
        (Conflict::Skip, vec!["/b/x.pdf", "/dl/x.pdf"]),
        (Conflict::Overwrite, vec!["/b/x.pdf"]),
    ] {
        let mut fs = MemFs::new();
        fs.add_file("/b/x.pdf", 1, 0);
        fs.add_file("/dl/x.pdf", 100, 5);
        let mut j = MemJournal::new();
        let a = Action::Move { dest: t("/b"), conflict: c, keep_structure: false, remove_empty: false };
        let r = run_rule(&mut fs, &mut j, &rule(vec![a]), &meta("/dl/x.pdf"), &ctx(&d));
        assert!(r.ok());
        // Comparăm căi, nu text: pe Windows `join` pune `\`, iar `PathBuf` le tratează egal.
        let mut got = fs.file_paths();
        got.sort();
        let mut want: Vec<PathBuf> = expect.iter().map(|x| p(x)).collect();
        want.sort();
        assert_eq!(got, want, "{c:?}");
    }
}

#[test]
fn keep_structure_and_remove_empty() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a/b/x.pdf", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let a = Action::Move { dest: t("/out"), conflict: Conflict::Rename, keep_structure: true, remove_empty: true };
    let m = FileMeta::new("/dl/a/b/x.pdf", "/dl", 100, 0, 0);
    let r = run_rule(&mut fs, &mut j, &rule(vec![a]), &m, &ctx(&d));
    assert!(r.ok());
    assert_eq!(r.path, p("/out/a/b/x.pdf"));
    assert!(!fs.has_dir("/dl/a"));
    assert!(fs.has_dir("/dl"));
}

#[test]
fn hostile_name_cannot_escape() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.pdf", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let a = Action::Rename { name: t("../{name}/.."), conflict: Conflict::Rename };
    let r = run_rule(&mut fs, &mut j, &rule(vec![a]), &meta("/dl/x.pdf"), &ctx(&d));
    assert!(!r.ok());
    assert_eq!(fs.file_paths(), vec![p("/dl/x.pdf")]);
}

#[test]
fn copy_then_target_and_change_ext() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.txt", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let acts = vec![
        Action::Copy { dest: t("/bk"), conflict: Conflict::Overwrite, keep_structure: false, then_target: true },
        Action::ChangeExtension { ext: t(".bak") },
    ];
    let r = run_rule(&mut fs, &mut j, &rule(acts), &meta("/dl/x.txt"), &ctx(&d));
    assert!(r.ok(), "{:?}", r.steps);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/bk/x.bak"), p("/dl/x.txt")]);
}

#[test]
fn run_failure_stops_the_rule() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.txt", 100, 5);
    fs.run_exit = 3;
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let acts = vec![Action::Run { program: t("tool"), args: vec![t("{path}")] }, mv("/b")];
    let r = run_rule(&mut fs, &mut j, &rule(acts), &meta("/dl/x.txt"), &ctx(&d));
    assert!(!r.ok());
    assert_eq!(r.steps.len(), 2 - 1);
    assert_eq!(fs.ran, vec![("tool".to_string(), vec!["/dl/x.txt".to_string()])]);
    assert_eq!(fs.file_paths(), vec![p("/dl/x.txt")]);
}

#[test]
fn trash_marks_gone_and_undo_restores() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.txt", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let r = run_rule(&mut fs, &mut j, &rule(vec![Action::Trash { remove_empty: false }]), &meta("/dl/x.txt"), &ctx(&d));
    assert!(r.gone && r.ok());
    let out = undo_run(&mut fs, &mut j, r.run_id, false);
    assert_eq!(out.len(), 1);
    assert!(fs.stat("/dl/x.txt").is_some());
}

#[test]
fn undo_whole_run_restores_original() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/factura.pdf", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let r = run_rule(&mut fs, &mut j, &invoice_rule(), &meta("/dl/factura.pdf"), &ctx(&d));
    let res = undo_run(&mut fs, &mut j, r.run_id, false);
    assert!(res.iter().all(|(_, o)| matches!(o, Ok(UndoOutcome::Restored(_)))));
    assert_eq!(fs.file_paths(), vec![p("/dl/factura.pdf")]);
}

/// Oprire bruscă la fiecare operație posibilă: fișierul trebuie să existe exact o dată,
/// intact, fără copii parțiale.
#[test]
fn crash_at_every_step_never_loses_or_duplicates() {
    for cross in [false, true] {
        for n in 0..12 {
            let mut fs = MemFs::new();
            if cross {
                fs.set_volumes(&["/dl", "/arhiva"]);
            }
            fs.add_file("/dl/factura.pdf", 100, 5);
            let mut j = MemJournal::new();
            let d = HashMap::new();
            fs.fail_at(n);
            let _ = run_rule(&mut fs, &mut j, &invoice_rule(), &meta("/dl/factura.pdf"), &ctx(&d));
            j.simulate_crash();
            fs.clear_failure();
            recover(&mut fs, &mut j);
            let files = fs.file_paths();
            assert_eq!(files.len(), 1, "cross={cross} n={n}: {files:?}");
            assert_eq!(fs.stat(&files[0]).unwrap().size, 100, "cross={cross} n={n}");
            assert!(j.in_progress().is_empty());
        }
    }
}

#[test]
fn torn_cross_device_move_keeps_source() {
    let mut fs = MemFs::new();
    fs.set_volumes(&["/dl", "/arhiva"]);
    fs.add_file("/dl/x.pdf", 100, 5);
    fs.torn_next_copy();
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let r = run_rule(&mut fs, &mut j, &rule(vec![mv("/arhiva")]), &meta("/dl/x.pdf"), &ctx(&d));
    assert!(!r.ok());
    assert_eq!(fs.file_paths(), vec![p("/dl/x.pdf")]);
    assert_eq!(fs.stat("/dl/x.pdf").unwrap().size, 100);
}

/// Previzualizarea arată destinația reală (variabile, `_2` între fișierele previzualizate)
/// fără să atingă discul și fără să ruleze comenzi.
#[test]
fn plan_shows_destination_without_touching_disk() {
    let d = HashMap::new();
    let mut real = MemFs::new();
    real.add_file("/dl/factura.pdf", 100, timestamp(2025, 3, 7, 9, 0, 0));
    real.add_file("/arhiva/2025/factura.pdf", 1, 1);
    let before = real.file_paths();
    let r = rule(vec![
        mv("/arhiva/{modified:AAAA}"),
        Action::Run { program: t("unealta"), args: vec![t("{path}")] },
    ]);
    let mut dry = org_fs::DryFs::new(&real);
    let m = FileMeta::new("/dl/factura.pdf", "/dl", 100, 0, timestamp(2025, 3, 7, 9, 0, 0));
    let plan = plan_rule(&mut dry, &r, &m, &ctx(&d));
    assert_eq!(plan.path, p("/arhiva/2025/factura_2.pdf"));
    assert_eq!(plan.ops, vec![(OpKind::Move, Some(p("/arhiva/2025/factura_2.pdf"))), (OpKind::Run, None)]);
    assert!(!plan.gone && plan.error.is_none());
    assert_eq!(real.file_paths(), before);
    assert!(real.ran.is_empty());

    // în aceeași simulare fișierul a plecat deja: o a doua regulă nu-l mai găsește
    let trash = rule(vec![Action::Trash { remove_empty: false }]);
    assert!(plan_rule(&mut dry, &trash, &meta("/dl/factura.pdf"), &ctx(&d)).error.is_some());
    let mut fresh = org_fs::DryFs::new(&real);
    assert!(plan_rule(&mut fresh, &trash, &meta("/dl/factura.pdf"), &ctx(&d)).gone);
    assert_eq!(real.file_paths(), before);
}

#[test]
fn reserved_and_trailing_names_are_made_safe() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.txt", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let acts = vec![mv("/out/Doc. "), Action::Rename { name: t("nul"), conflict: Conflict::Rename }];
    let r = run_rule(&mut fs, &mut j, &rule(acts), &meta("/dl/x.txt"), &ctx(&d));
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(fs.file_paths(), vec![p("/out/Doc/_nul.txt")]);
}

#[test]
fn command_goes_through_the_shell_with_quoted_values() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a & b.txt", 100, 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let acts = vec![Action::Command { line: t("type {path} >> log.txt") }];
    let r = run_rule(&mut fs, &mut j, &rule(acts), &meta("/dl/a & b.txt"), &ctx(&d));
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(fs.ran, vec![("<shell>".to_string(), vec!["type \"/dl/a & b.txt\" >> log.txt".to_string()])]);
    assert_eq!(j.entries()[0].kind, OpKind::Run);
}
