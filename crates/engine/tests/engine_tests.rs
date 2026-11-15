use std::collections::HashMap;
use std::path::{Path, PathBuf};

use org_core::timefmt::{timestamp, Lang};
use org_core::*;
use org_engine::*;
use org_fs::{FileStat, Fs, MemFs};
use org_journal::{Journal, MemJournal, OpKind};

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}
fn t(s: &str) -> Template {
    Template::parse(s).unwrap()
}

fn pdf_rule(id: u64) -> Rule {
    Rule {
        id,
        name: "pdf".into(),
        enabled: true,
        hidden: false,
        folders: vec![WatchedFolder { path: p("/dl"), recursive: true }],
        condition: Condition::All(vec![Condition::Text {
            field: TextField::Extension,
            op: TextOp::Is,
            value: "pdf".into(),
        }]),
        actions: vec![Action::Move {
            dest: t("/pdf"),
            conflict: Conflict::Rename,
            keep_structure: false,
            remove_empty: false,
        }],
        continue_after_move: false, simulate: false,
    }
}

fn ctx(d: &HashMap<String, Dictionary>, now: i64) -> RenderCtx<'_> {
    RenderCtx { now, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: d, content: None, raw: None }
}

const T0: i64 = 1_800_000_000;

#[test]
fn scan_moves_matching_files_and_ignores_the_rest() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a.pdf", 10, 1);
    fs.add_file("/dl/sub/b.PDF", 10, 1);
    fs.add_file("/dl/c.txt", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![pdf_rule(1)], 0);
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(r.seen, 3);
    assert_eq!(r.processed.len(), 2);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/dl/c.txt"), p("/pdf/a.pdf"), p("/pdf/b.PDF")]);
}

#[test]
fn second_scan_does_nothing_and_modified_file_is_reprocessed() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/c.txt", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![pdf_rule(1)], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    // Fișierele care nu se potrivesc nu se memorează (se reevaluează ieftin la nevoie).
    assert_eq!(e.state.len(), 0);
    // același fișier: nu se procesează
    assert!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).processed.is_empty());
    // devine pdf (redenumit și modificat) → se procesează
    fs.add_file("/dl/c.pdf", 11, 2);
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(r.processed.len(), 1);
}

#[test]
fn pending_forces_reprocessing() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/x.pdf", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut rule = pdf_rule(1);
    rule.actions = vec![Action::Copy {
        dest: t("/bk"),
        conflict: Conflict::Overwrite,
        keep_structure: false,
        then_target: false,
    }];
    let mut e = Engine::new(vec![rule], 0);
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).processed.len(), 1);
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).processed.len(), 0);
    e.state.mark_pending(Path::new("/dl/x.pdf"));
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).processed.len(), 1);
}

#[test]
fn temp_downloads_are_never_touched() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/film.mp4.crdownload", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut rule = pdf_rule(1);
    rule.condition = Condition::AllFiles;
    let mut e = Engine::new(vec![rule], 0);
    assert!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).processed.is_empty());
    assert!(fs.stat("/dl/film.mp4.crdownload").is_some());
}

#[test]
fn file_still_changing_waits_until_stable() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![pdf_rule(1)], 5);
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0)).waiting, 1);
    // încă 2 s: nu e destul
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0 + 2)).waiting, 1);
    // se mai scrie în el: contorul pornește de la capăt
    fs.add_file("/dl/a.pdf", 20, 2);
    assert_eq!(e.scan(&mut fs, &mut j, &ctx(&d, T0 + 6)).waiting, 1);
    assert!(fs.stat("/dl/a.pdf").is_some());
    // liniște destulă
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0 + 12));
    assert_eq!(r.processed.len(), 1);
    assert!(fs.stat("/pdf/a.pdf").is_some());
}

#[test]
fn rename_in_same_folder_does_not_loop() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut rule = pdf_rule(1);
    rule.actions = vec![Action::Rename { name: t("x_{name}"), conflict: Conflict::Rename }];
    let mut e = Engine::new(vec![rule], 0);
    for _ in 0..3 {
        e.scan(&mut fs, &mut j, &ctx(&d, T0));
    }
    assert_eq!(fs.file_paths(), vec![p("/dl/x_a.pdf")]);
}

#[test]
fn moved_file_is_not_seen_by_later_rules_unless_still_watched() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut second = pdf_rule(2);
    second.actions = vec![Action::Delete { remove_empty: false }];
    let mut e = Engine::new(vec![pdf_rule(1), second], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.file_paths(), vec![p("/pdf/a.pdf")]);
}

#[test]
fn unavailable_folder_is_just_skipped() {
    let mut fs = MemFs::new();
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![pdf_rule(1)], 0);
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(r.seen, 0);
}

#[test]
fn stability_tracker_basics() {
    let mut s = StabilityTracker::new();
    let st = FileStat { size: 1, modified: 1 };
    assert!(!s.is_stable(Path::new("/a"), st, 100, 5));
    assert!(!s.is_stable(Path::new("/a"), st, 104, 5));
    assert!(s.is_stable(Path::new("/a"), st, 105, 5));
    assert!(!s.is_stable(Path::new("/a"), FileStat { size: 2, modified: 1 }, 106, 5));
}

#[test]
fn std_fs_scan_on_a_real_dir() {
    let base = std::env::temp_dir().join(format!("org-engine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("dl")).unwrap();
    std::fs::write(base.join("dl/a.pdf"), b"x").unwrap();
    std::fs::write(base.join("dl/b.txt"), b"y").unwrap();
    let mut rule = pdf_rule(1);
    rule.folders = vec![WatchedFolder { path: base.join("dl"), recursive: true }];
    rule.actions = vec![Action::Move {
        dest: Template::literal(base.join("out").to_str().unwrap()),
        conflict: Conflict::Rename,
        keep_structure: false,
        remove_empty: false,
    }];
    let mut fs = org_fs::StdFs;
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let now = timestamp(2026, 10, 10, 0, 0, 0);
    let mut e = Engine::new(vec![rule], 0);
    let r = e.scan(&mut fs, &mut j, &ctx(&d, now));
    assert_eq!(r.processed.len(), 1);
    assert!(base.join("out/a.pdf").exists());
    assert!(base.join("dl/b.txt").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn state_changes_are_logged_once_and_can_be_reloaded() {
    let mut st = FileState::new();
    let a = FileStat { size: 1, modified: 1 };
    st.mark_done(1, Path::new("/a"), a);
    st.mark_done(1, Path::new("/a"), a); // neschimbat: nu se înregistrează din nou
    st.mark_done(2, Path::new("/a"), a);
    st.mark_pending(Path::new("/a"));
    st.mark_pending(Path::new("/nu-exista"));
    assert_eq!(
        st.take_changes(),
        vec![
            StateChange::Set(1, p("/a"), a),
            StateChange::Set(2, p("/a"), a),
            StateChange::ClearPath(p("/a")),
        ]
    );
    assert!(st.take_changes().is_empty());
    assert!(st.is_empty());
    st.load(1, p("/b"), a);
    assert!(st.is_done(1, Path::new("/b"), a));
    assert!(st.take_changes().is_empty(), "load nu e o schimbare");
}

#[test]
fn content_conditions_and_dictionaries_read_each_file_once() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/a.txt", "Factură nr. 12 de la Furnizor SRL".as_bytes(), 1);
    fs.add_file_with("/dl/b.txt", "Ofertă de preț".as_bytes(), 1);
    fs.add_file_with("/dl/c.jpg", b"FACTURA", 1);
    let mut d = HashMap::new();
    d.insert(
        "Tip".to_string(),
        Dictionary { field: TextField::Content, entries: vec![("factură, invoice".into(), "Facturi".into())], default: Some("Altele".into()), extract: None },
    );
    let mut r1 = pdf_rule(1);
    r1.folders[0].recursive = false;
    r1.condition = Condition::All(vec![
        Condition::Text { field: TextField::Content, op: TextOp::Contains, value: "FACTURĂ".into() },
        Condition::Text { field: TextField::Extension, op: TextOp::IsNot, value: "zip".into() },
    ]);
    r1.actions = vec![Action::Move { dest: t("/arhiva/{var:Tip}"), conflict: Conflict::Rename, keep_structure: false, remove_empty: false }];
    // A doua regulă citește tot conținutul: tot o singură citire pe fișier.
    let mut r2 = pdf_rule(2);
    r2.folders[0].recursive = false;
    r2.condition = Condition::All(vec![Condition::Text { field: TextField::Content, op: TextOp::DoesNotContain, value: "nimic".into() }]);
    r2.actions = vec![Action::Copy { dest: t("/copie/{var:Tip}"), conflict: Conflict::Rename, keep_structure: false, then_target: false }];
    r2.continue_after_move = true;
    let mut e = Engine::new(vec![r2, r1], 0);
    let mut j = MemJournal::new();
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let mut f = fs.file_paths();
    f.sort();
    // Imaginea nu are text: „nu conține” e adevărat, dicționarul dă valoarea implicită.
    assert_eq!(
        f,
        vec![p("/arhiva/Facturi/a.txt"), p("/copie/Altele/b.txt"), p("/copie/Altele/c.jpg"), p("/copie/Facturi/a.txt"), p("/dl/b.txt"), p("/dl/c.jpg")]
    );
    // a.txt: o citire (ambele reguli + cele două dicționare); b.txt: una; c.jpg: niciuna (fără text).
    assert_eq!(fs.reads.get(), 2);
}

#[test]
fn files_in_use_or_still_downloading_wait_without_errors() {
    let mut fs = MemFs::new();
    // Firefox: locțiitor gol + .part; Word: document deschis; aria2: fișierul de control.
    fs.add_file("/dl/firefox.pdf", 0, 1);
    fs.add_file("/dl/firefox.pdf.part", 500, 1);
    fs.add_file("/dl/deschis.pdf", 10, 1);
    fs.busy.insert(p("/dl/deschis.pdf"));
    fs.add_file("/dl/aria.pdf", 10, 1);
    fs.add_file("/dl/aria.pdf.aria2", 1, 1);
    fs.add_file("/dl/torrent.pdf.!qB", 10, 1);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![pdf_rule(1)], 0);
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(r.processed.is_empty());
    assert_eq!(r.waiting, 3);
    assert!(j.entries().is_empty(), "fără erori în jurnal");
    // Descărcările se termină, documentul se închide.
    fs.remove_file(&p("/dl/firefox.pdf.part")).unwrap();
    fs.add_file("/dl/firefox.pdf", 500, 2);
    fs.busy.clear();
    fs.remove_file(&p("/dl/aria.pdf.aria2")).unwrap();
    fs.rename(&p("/dl/torrent.pdf.!qB"), &p("/dl/torrent.pdf")).unwrap();
    let r = e.scan(&mut fs, &mut j, &ctx(&d, T0 + 1));
    assert_eq!(r.processed.len(), 4);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/pdf/aria.pdf"), p("/pdf/deschis.pdf"), p("/pdf/firefox.pdf"), p("/pdf/torrent.pdf")]);
    assert_eq!(fs.metadata(&p("/pdf/firefox.pdf")).unwrap().size, 500);
}

#[test]
fn content_is_not_read_while_the_file_still_changes() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/a.txt", b"factura", 1);
    let mut r = pdf_rule(1);
    r.condition = Condition::All(vec![Condition::Text { field: TextField::Content, op: TextOp::Contains, value: "factura".into() }]);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![r], 5);
    assert!(matches!(e.process_path(&mut fs, &mut j, &ctx(&d, T0), &p("/dl/a.txt"), &p("/dl")), Outcome::Waiting));
    assert_eq!(fs.reads.get(), 0, "nimic citit cât fișierul nu e stabil");
    assert!(matches!(e.process_path(&mut fs, &mut j, &ctx(&d, T0 + 6), &p("/dl/a.txt"), &p("/dl")), Outcome::Ran(_)));
    assert_eq!(fs.reads.get(), 1);
}

fn dup_rule(id: u64, actions: Vec<Action>) -> Rule {
    let mut r = pdf_rule(id);
    r.name = "dubluri".into();
    r.condition = Condition::All(vec![Condition::Duplicate { is: true, within: vec![] }]);
    r.actions = actions;
    r
}

#[test]
fn duplicates_of_an_older_file_in_the_same_folder_go_to_trash() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/raport.pdf", b"continut A", 1);
    fs.add_file_with("/dl/raport (1).pdf", b"continut A", 2);
    fs.add_file_with("/dl/raport (2).pdf", b"continut A", 3);
    // Aceeași dimensiune, alt conținut; și aceeași valoare într-un subfolder (alt folder).
    fs.add_file_with("/dl/altul.pdf", b"continut B", 4);
    fs.add_file_with("/dl/sub/raport.pdf", b"continut A", 5);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/raport (1).pdf"), p("/dl/raport (2).pdf")]);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/dl/altul.pdf"), p("/dl/raport.pdf"), p("/dl/sub/raport.pdf")]);
}

#[test]
fn scan_lists_each_folder_once_for_duplicates_and_reports_progress() {
    let mut fs = MemFs::new();
    for i in 0..200 {
        // 20 de dimensiuni diferite; în fiecare grup, câte o copie identică a primului fișier.
        let body = format!("{:0w$}", i % 10, w = 10 + i % 20);
        fs.add_file_with(format!("/dl/f{i:03}.pdf"), body.as_bytes(), i as i64);
    }
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    let mut seen = Vec::new();
    let rep = e.scan_with(&mut fs, &mut j, &ctx(&d, T0), &mut |done, total| seen.push((done, total)));
    assert_eq!(rep.seen, 200);
    // O listare pentru scanare și una pentru dubluri, nu una pe fișier.
    assert_eq!(fs.lists.get(), 2);
    assert_eq!(seen.first(), Some(&(0, 200)));
    assert_eq!(seen.last(), Some(&(200, 200)));
    assert_eq!(seen.len(), 201);
    // 20 de conținuturi, fiecare de 10 ori: rămâne câte unul, cel mai vechi (f000..f019).
    let mut kept: Vec<String> = fs.file_paths().iter().map(|x| fs_body(&fs, x)).collect();
    kept.sort();
    kept.dedup();
    assert_eq!(kept.len(), 20);
    let names: Vec<PathBuf> = (0..20).map(|i| p(&format!("/dl/f{i:03}.pdf"))).collect();
    assert_eq!(fs.file_paths(), names);
    assert_eq!(fs.trashed().len(), 180);
}

#[test]
fn many_files_of_the_same_size_are_not_compared_pairwise() {
    let mut fs = MemFs::new();
    for i in 0..100 {
        fs.add_file_with(format!("/dl/f{i:03}.bin"), format!("{i:08}").as_bytes(), i as i64);
    }
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
    // O amprentă pe fișier, nu 2 citiri pentru fiecare din cele 4 950 de perechi.
    assert!(fs.reads.get() <= 100, "citiri: {}", fs.reads.get());
}

fn fs_body(fs: &MemFs, path: &Path) -> String {
    String::from_utf8(fs.read_prefix(path, 1 << 20).unwrap()).unwrap()
}

#[test]
fn dedupe_conflict_trashes_identical_and_renames_different() {
    let mut fs = MemFs::new();
    fs.add_file_with("/pdf/a.pdf", b"aceeasi", 1);
    fs.add_file_with("/pdf/b.pdf", b"veche", 1);
    fs.add_file_with("/dl/a.pdf", b"aceeasi", 5);
    fs.add_file_with("/dl/b.pdf", b"noua!", 5);
    let mut r = pdf_rule(1);
    r.actions = vec![Action::Move { dest: t("/pdf"), conflict: Conflict::Dedupe, keep_structure: false, remove_empty: false }];
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/a.pdf")]);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/pdf/a.pdf"), p("/pdf/b.pdf"), p("/pdf/b_2.pdf")]);
    // Dublura e în jurnal ca „Coș”, cu originalul ca destinație, și se poate restaura.
    let t = j.entries().iter().find(|x| x.kind == OpKind::Trash).unwrap();
    assert_eq!((t.src.clone(), t.dst.clone()), (p("/dl/a.pdf"), Some(p("/pdf/a.pdf"))));
}

#[test]
fn confirmation_holds_the_file_until_approved_or_rejected() {
    let mk = || {
        let mut fs = MemFs::new();
        fs.add_file_with("/dl/x.pdf", b"aceeasi", 1);
        fs.add_file_with("/dl/x (1).pdf", b"aceeasi", 2);
        fs
    };
    let rules = || {
        let mut move_pdf = pdf_rule(2);
        move_pdf.folders[0].recursive = false;
        vec![dup_rule(1, vec![Action::Confirm, Action::Trash { remove_empty: false }]), move_pdf]
    };
    let d = HashMap::new();

    // Aprobare: dublura merge în Coș; până atunci nicio altă regulă nu o atinge.
    let mut fs = mk();
    let mut j = MemJournal::new();
    let mut e = Engine::new(rules(), 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let pend = j.pending();
    assert_eq!(pend.len(), 1);
    assert_eq!((pend[0].path.clone(), pend[0].step), (p("/dl/x (1).pdf"), 1));
    assert_eq!(fs.file_paths(), vec![p("/dl/x (1).pdf"), p("/pdf/x.pdf")], "originalul mutat, dublura așteaptă");
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 10));
    assert_eq!(j.pending().len(), 1, "nu se întreabă de două ori");
    assert!(fs.exists(&p("/dl/x (1).pdf")));
    e.approve(&mut fs, &mut j, &ctx(&d, T0 + 20), &pend[0]);
    assert_eq!(fs.trashed(), vec![p("/dl/x (1).pdf")]);
    assert!(j.pending().is_empty());

    // Respingere: rămâne, iar regula următoare îl ia.
    let mut fs = mk();
    let mut j = MemJournal::new();
    let mut e = Engine::new(rules(), 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let pend = j.pending();
    e.reject(&mut fs, &mut j, &ctx(&d, T0 + 20), &pend[0]);
    assert!(fs.trashed().is_empty());
    assert_eq!(fs.file_paths(), vec![p("/pdf/x (1).pdf"), p("/pdf/x.pdf")]);
    // Fișierul schimbat între timp: se reevaluează de la început (și se întreabă iar).
    let mut fs = mk();
    let mut j = MemJournal::new();
    let mut e = Engine::new(rules(), 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let pend = j.pending();
    fs.add_file_with("/dl/x (1).pdf", b"altceva", 9);
    e.approve(&mut fs, &mut j, &ctx(&d, T0 + 20), &pend[0]);
    assert!(fs.trashed().is_empty(), "nu se aplică pe un fișier schimbat");
}

#[test]
fn dedupe_finds_identical_files_under_other_names_in_the_destination() {
    // Originalul a fost deja sortat; copia descărcată mai târziu are alt nume.
    let mut fs = MemFs::new();
    fs.add_file_with("/pdf/raport.pdf", b"acelasi continut", 1);
    fs.add_file_with("/dl/raport (1).pdf", b"acelasi continut", 5);
    fs.add_file_with("/dl/altul.pdf", b"alt continut!!!!", 5);
    let mut r = pdf_rule(1);
    r.actions = vec![Action::Move { dest: t("/pdf"), conflict: Conflict::Dedupe, keep_structure: false, remove_empty: false }];
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/raport (1).pdf")]);
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/pdf/altul.pdf"), p("/pdf/raport.pdf")]);
}

#[test]
fn simulated_rule_only_writes_the_journal_and_runs_for_real_when_turned_off() {
    use org_journal::Status;
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/a.pdf", b"x", 1);
    let mut r = pdf_rule(1);
    r.simulate = true;
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![r.clone()], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.file_paths(), vec![p("/dl/a.pdf")], "nimic mutat");
    assert_eq!(j.entries().len(), 1);
    assert_eq!(j.entries()[0].status, Status::Simulated);
    assert_eq!(j.entries()[0].dst, Some(p("/pdf/a.pdf")));
    assert!(j.in_progress().is_empty());
    // A doua trecere nu mai scrie nimic.
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 10));
    assert_eq!(j.entries().len(), 1);
    // Oprită simularea: aceeași regulă (același id) mută de-adevăratelea.
    r.simulate = false;
    e.set_rules(vec![r]);
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 20));
    assert_eq!(fs.file_paths(), vec![p("/pdf/a.pdf")]);
    assert_eq!(j.entries().last().unwrap().status, Status::Done);
}

#[test]
fn simulation_goes_past_confirmation_and_logs_the_trash() {
    use org_journal::Status;
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/x.pdf", b"aceeasi", 1);
    fs.add_file_with("/dl/x (1).pdf", b"aceeasi", 2);
    let mut r = dup_rule(1, vec![Action::Confirm, Action::Trash { remove_empty: false }]);
    r.simulate = true;
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(j.pending().is_empty(), "în simulare nu se cere nimic");
    assert!(fs.trashed().is_empty());
    let es = j.entries();
    assert_eq!(es.len(), 1, "{es:?}");
    assert_eq!((es[0].kind, es[0].status.clone(), es[0].src.clone()), (OpKind::Trash, Status::Simulated, p("/dl/x (1).pdf")));
}

#[test]
fn pending_disappears_when_the_rule_stops_asking() {
    let mk = || {
        let mut fs = MemFs::new();
        fs.add_file_with("/dl/x.pdf", b"aceeasi", 1);
        fs.add_file_with("/dl/x (1).pdf", b"aceeasi", 2);
        fs
    };
    let rule = || dup_rule(1, vec![Action::Confirm, Action::Trash { remove_empty: false }]);
    let d = HashMap::new();
    for change in ["simulare", "oprită", "ștearsă", "fără confirmare"] {
        let mut fs = mk();
        let mut j = MemJournal::new();
        let mut e = Engine::new(vec![rule()], 0);
        e.scan(&mut fs, &mut j, &ctx(&d, T0));
        assert_eq!(j.pending().len(), 1);
        let mut r = rule();
        let rules = match change {
            "simulare" => { r.simulate = true; vec![r] }
            "oprită" => { r.enabled = false; vec![r] }
            "ștearsă" => vec![],
            _ => { r.actions = vec![Action::Trash { remove_empty: false }]; vec![r] }
        };
        e.set_rules(rules);
        assert_eq!(e.drop_stale_pending(&mut j), 1, "{change}");
        assert!(j.pending().is_empty(), "{change}");
        // Regula a uitat fișierul: îl reevaluează (simulare: simulat; fără confirmare: Coș).
        e.scan(&mut fs, &mut j, &ctx(&d, T0 + 10));
        match change {
            "fără confirmare" => assert_eq!(fs.trashed(), vec![p("/dl/x (1).pdf")]),
            _ => assert!(fs.trashed().is_empty(), "{change}"),
        }
    }
    // Neschimbată: rămâne.
    let mut fs = mk();
    let mut j = MemJournal::new();
    let mut e = Engine::new(vec![rule()], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    e.set_rules(vec![rule()]);
    assert_eq!(e.drop_stale_pending(&mut j), 0);
    assert_eq!(j.pending().len(), 1);
}

fn dup_rule_within(id: u64, within: Vec<WatchedFolder>) -> Rule {
    let mut r = dup_rule(id, vec![Action::Trash { remove_empty: false }]);
    r.condition = Condition::All(vec![Condition::Duplicate { is: true, within }]);
    r
}

#[test]
fn the_original_can_live_in_another_folder() {
    let setup = || {
        let mut fs = MemFs::new();
        // Descărcat și sortat de mult în Documente\2025; acum descărcat din nou.
        fs.add_file_with("/docs/2025/raport.pdf", b"acelasi continut", 1);
        fs.add_file_with("/dl/raport.pdf", b"acelasi continut", 5);
        fs.add_file_with("/dl/altul.pdf", b"alt continut  xx", 5);
        fs
    };
    let d = HashMap::new();
    // Fără folderele suplimentare: doar același folder, deci nu e dublură.
    let mut fs = setup();
    let mut j = MemJournal::new();
    Engine::new(vec![dup_rule_within(1, vec![])], 0).scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
    // Cu Documente (cu subfoldere): descărcarea nouă merge în Coș, originalul rămâne.
    let mut fs = setup();
    let mut j = MemJournal::new();
    let w = vec![WatchedFolder { path: p("/docs"), recursive: true }];
    Engine::new(vec![dup_rule_within(1, w)], 0).scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/raport.pdf")]);
    assert!(fs.exists(&p("/docs/2025/raport.pdf")));
    // Fără subfoldere, 2025 nu se vede.
    let mut fs = setup();
    let mut j = MemJournal::new();
    let w = vec![WatchedFolder { path: p("/docs"), recursive: false }];
    Engine::new(vec![dup_rule_within(1, w)], 0).scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
}

#[test]
fn the_older_copy_stays_even_when_it_is_the_one_being_watched() {
    // Copia din folderul urmărit e cea veche: ea e originalul, nu se atinge.
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/raport.pdf", b"acelasi continut", 1);
    fs.add_file_with("/docs/raport.pdf", b"acelasi continut", 9);
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let w = vec![WatchedFolder { path: p("/docs"), recursive: true }];
    Engine::new(vec![dup_rule_within(1, w)], 0).scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
}

#[test]
fn prints_are_kept_between_runs_and_renewed_when_the_file_changes() {
    let mut fs = MemFs::new();
    // Aceeași dimensiune, conținut diferit: trebuie citite amprentele.
    let big = |c: u8| vec![c; 200_000];
    fs.add_file_with("/dl/a.bin", &big(1), 1);
    fs.add_file_with("/dl/b.bin", &big(2), 2);
    let d = HashMap::new();
    let mut e = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    let mut j = MemJournal::new();
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
    let rows = e.prints().take_changes();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(e.prints().take_changes().is_empty(), "o singură dată");
    // Altă pornire, cu catalogul salvat: nicio citire.
    fs.reads.set(0);
    let mut e2 = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    e2.set_prints(Prints::from_rows(rows.clone()));
    e2.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.reads.get(), 0);
    assert!(e2.prints().take_changes().is_empty());
    // b.bin devine copia lui a.bin (aceeași dimensiune, altă dată): amprenta se reface.
    fs.add_file_with("/dl/b.bin", &big(1), 3);
    let mut e3 = Engine::new(vec![dup_rule(1, vec![Action::Trash { remove_empty: false }])], 0);
    e3.set_prints(Prints::from_rows(rows));
    e3.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/b.bin")]);
    // Curățenia: rândurile fișierelor dispărute.
    assert_eq!(e3.prints().remove_missing(&fs), vec![p("/dl/b.bin")]);
}

#[test]
fn groups_list_every_set_of_identical_files_biggest_first() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/film.mkv", &vec![7u8; 300_000], 1);
    fs.add_file_with("/dl/sub/film (1).mkv", &vec![7u8; 300_000], 2);
    fs.add_file_with("/arh/film.mkv", &vec![7u8; 300_000], 3);
    // Aceeași dimensiune, alt conținut (diferă după primii 64 KB).
    let mut other = vec![7u8; 300_000];
    other[250_000] = 1;
    fs.add_file_with("/dl/alt.mkv", &other, 1);
    fs.add_file_with("/dl/poza.jpg", b"poza", 1);
    fs.add_file_with("/dl/poza (1).jpg", b"poza", 2);
    fs.add_file_with("/dl/gol1.txt", b"", 1);
    fs.add_file_with("/dl/gol2.txt", b"", 1);
    let c = DirCache::default();
    let folders = vec![WatchedFolder { path: p("/dl"), recursive: true }, WatchedFolder { path: p("/arh"), recursive: false }];
    let mut steps = Vec::new();
    let g = c.groups(&fs, &folders, 0, &mut |s, _, _| { steps.push(s); true }).unwrap();
    assert_eq!(g.len(), 2, "{g:?}");
    assert_eq!(g[0].files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        vec![p("/dl/film.mkv"), p("/dl/sub/film (1).mkv"), p("/arh/film.mkv")]);
    assert_eq!(g[0].wasted(), 600_000);
    assert_eq!(g[1].files.len(), 2);
    assert!(steps.contains(&GroupStep::Full));
    // Pragul de dimensiune: doar filmele.
    assert_eq!(c.groups(&fs, &folders, 1000, &mut |_, _, _| true).unwrap().len(), 1);
    // Oprit din progres.
    assert!(c.groups(&fs, &folders, 0, &mut |s, _, _| s == GroupStep::Listing).is_none());
}

#[test]
fn content_rules_do_not_reread_unchanged_files_after_a_restart() {
    let mut fs = MemFs::new();
    for i in 0..20 {
        fs.add_file_with(format!("/dl/doc{i}.txt"), format!("oferta numarul {i}").as_bytes(), 1);
    }
    let mut r = pdf_rule(1);
    r.folders[0].recursive = false;
    r.condition = Condition::All(vec![Condition::Text { field: TextField::Content, op: TextOp::Contains, value: "factura".into() }]);
    r.actions = vec![Action::Trash { remove_empty: false }];
    let d = HashMap::new();
    let mut e = Engine::new(vec![r.clone()], 0);
    e.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert!(fs.trashed().is_empty());
    let first = fs.reads.get();
    assert!(first >= 20);
    let verdicts = e.prints().take_verdict_changes();
    assert_eq!(verdicts.len(), 20);
    // Repornire cu catalogul salvat: nicio citire.
    fs.reads.set(0);
    let mut e2 = Engine::new(vec![r.clone()], 0);
    e2.set_prints(Prints::default().with_verdicts(verdicts.clone()));
    e2.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.reads.get(), 0);
    // Un fișier schimbat se citește din nou și acum se potrivește.
    fs.add_file_with("/dl/doc3.txt", b"factura 3", 2);
    let mut e3 = Engine::new(vec![r.clone()], 0);
    e3.set_prints(Prints::default().with_verdicts(verdicts.clone()));
    e3.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/doc3.txt")]);
    // Altă valoare căutată = altă cheie: se citește din nou.
    fs.reads.set(0);
    let mut r2 = r.clone();
    r2.condition = Condition::All(vec![Condition::Text { field: TextField::Content, op: TextOp::Contains, value: "oferta".into() }]);
    r2.actions = vec![Action::Copy { dest: t("/copii"), conflict: Conflict::Rename, keep_structure: false, then_target: false }];
    let mut e4 = Engine::new(vec![r2], 0);
    e4.set_prints(Prints::default().with_verdicts(verdicts));
    e4.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert!(fs.reads.get() >= 19);
}

fn var(field: TextField, ex: Extract, default: Option<&str>) -> Dictionary {
    Dictionary { field, entries: Vec::new(), default: default.map(String::from), extract: Some(ex) }
}

/// Invoices filed by supplier and invoice date, both taken from the text; files without a
/// date are left alone; the supplier keeps the case written in the list.
#[test]
fn values_from_the_text_build_the_destination() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/a.txt", "FACTURA ENEL\nData emiterii: 03.10.2026\nScadenta: 30.10.2026".as_bytes(), 1);
    fs.add_file_with("/dl/b.txt", "Factura Digi din 7 noiembrie 2026".as_bytes(), 1);
    fs.add_file_with("/dl/c.txt", "Factura fara data".as_bytes(), 1);
    let mut d = HashMap::new();
    d.insert(
        "Supplier".to_string(),
        var(TextField::Content, Extract { kind: ExtractKind::Words(vec!["Enel".into(), "Digi".into()]), ..Extract::default() }, Some("Other")),
    );
    d.insert("Date".to_string(), var(TextField::Content, Extract { kind: ExtractKind::Date { format: "AAAA-LL".into() }, ..Extract::default() }, None));
    let mut r = pdf_rule(1);
    r.folders[0].recursive = false;
    r.condition = Condition::All(vec![Condition::Var { name: "Date".into(), found: true }]);
    r.actions = vec![Action::Move { dest: t("/facturi/{var:Supplier}/{var:Date}"), conflict: Conflict::Rename, keep_structure: false, remove_empty: false }];
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/dl/c.txt"), p("/facturi/Digi/2026-11/b.txt"), p("/facturi/Enel/2026-10/a.txt")]);
    assert_eq!(fs.reads.get(), 3, "each file is read once for the condition and both variables");
}

#[test]
fn variable_conditions_are_remembered_like_content_conditions() {
    let mut fs = MemFs::new();
    for i in 0..10 {
        fs.add_file_with(format!("/dl/doc{i}.txt"), format!("offer {i}").as_bytes(), 1);
    }
    let mut d = HashMap::new();
    d.insert("No".to_string(), var(TextField::Content, Extract { kind: ExtractKind::Pattern("INV-####".into()), ..Extract::default() }, None));
    let mut r = pdf_rule(1);
    r.folders[0].recursive = false;
    r.condition = Condition::All(vec![Condition::Var { name: "No".into(), found: true }]);
    r.actions = vec![Action::Trash { remove_empty: false }];
    let mut e = Engine::new(vec![r.clone()], 0);
    e.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    let verdicts = e.prints().take_verdict_changes();
    assert_eq!(verdicts.len(), 10);
    fs.reads.set(0);
    let mut e2 = Engine::new(vec![r.clone()], 0);
    e2.set_prints(Prints::default().with_verdicts(verdicts.clone()));
    e2.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.reads.get(), 0, "unchanged files are not read again");
    // A changed definition is a different key: the files are read again.
    d.insert("No".to_string(), var(TextField::Content, Extract { kind: ExtractKind::Pattern("offer #".into()), ..Extract::default() }, None));
    let mut e3 = Engine::new(vec![r], 0);
    e3.set_prints(Prints::default().with_verdicts(verdicts));
    e3.scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.trashed().len(), 10);
}

#[test]
fn a_missing_or_unknown_variable_finds_nothing() {
    let mut fs = MemFs::new();
    fs.add_file_with("/dl/a.txt", b"hello", 1);
    let d = HashMap::new();
    let mut r = pdf_rule(1);
    r.folders[0].recursive = false;
    r.condition = Condition::Any(vec![Condition::Var { name: "Nope".into(), found: false }]);
    r.actions = vec![Action::Trash { remove_empty: false }];
    Engine::new(vec![r], 0).scan(&mut fs, &mut MemJournal::new(), &ctx(&d, T0));
    assert_eq!(fs.trashed(), vec![p("/dl/a.txt")]);
}
