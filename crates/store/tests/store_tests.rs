use std::collections::HashMap;
use std::path::{Path, PathBuf};

use org_actions::run_rule;
use org_core::timefmt::{timestamp, Lang};
use org_core::*;
use org_engine::{FileState, StateChange};
use org_fs::{FileStat, MemFs};
use org_journal::*;
use org_store::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn temp_db(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("org-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("test.db")
}

fn new_entry(run: u64, kind: OpKind) -> NewEntry {
    NewEntry {
        run_id: run,
        rule_id: 42,
        ts: 1000,
        kind,
        src: p("/dl/ă ș.pdf"),
        dst: Some(p("/out/ă ș.pdf")),
        size: 100,
        mtime: 5,
    }
}

#[test]
fn journal_basics_match_the_in_memory_one() {
    let mut s = Store::open_in_memory().unwrap();
    let run = s.next_run_id();
    assert_eq!(run, 1);
    assert_eq!(s.next_run_id(), 2);
    let a = s.begin(new_entry(run, OpKind::Move));
    let b = s.begin(new_entry(run, OpKind::Rename));
    assert_eq!(s.in_progress().len(), 2);
    s.finish(a, Status::Done);
    s.finish(b, Status::Failed("eroare \"citată\"".into()));
    assert!(s.in_progress().is_empty());
    let e = s.get(b).unwrap();
    assert_eq!(e.status, Status::Failed("eroare \"citată\"".into()));
    assert_eq!(e.src, p("/dl/ă ș.pdf"));
    assert_eq!(e.dst, Some(p("/out/ă ș.pdf")));
    assert_eq!((e.size, e.mtime, e.rule_id, e.kind), (100, 5, 42, OpKind::Rename));
    assert_eq!(s.by_run(run).len(), 2);
    assert!(s.get(999).is_none());
    assert_eq!(s.count(), 2);
    assert_eq!(s.last_id(), b);
    assert_eq!(s.entries_after(a).len(), 1);
    assert_eq!(s.recent(1)[0].id, b);
}

#[test]
fn data_survives_reopening_and_run_ids_continue() {
    let path = temp_db("reopen");
    {
        let mut s = Store::open(&path).unwrap();
        let r = s.next_run_id();
        let id = s.begin(new_entry(r, OpKind::Copy));
        s.finish(id, Status::Done);
        s.begin(new_entry(r, OpKind::Move)); // rămâne în curs
        s.save_state_changes(&[StateChange::Set(1, p("/a"), FileStat { size: 1, modified: 2 })]).unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    assert_eq!(s.next_run_id(), 2, "numerotarea continuă");
    assert_eq!(s.in_progress().len(), 1);
    assert_eq!(s.load_file_state().unwrap(), vec![(1, p("/a"), FileStat { size: 1, modified: 2 })]);
}

#[test]
fn file_state_round_trip_through_engine_state() {
    let mut s = Store::open_in_memory().unwrap();
    let mut st = FileState::new();
    let a = FileStat { size: 1, modified: 1 };
    st.mark_done(1, Path::new("/a"), a);
    st.mark_done(2, Path::new("/a"), a);
    st.mark_done(1, Path::new("/b"), a);
    s.save_state_changes(&st.take_changes()).unwrap();
    assert_eq!(s.file_state_count(), 3);
    st.mark_pending(Path::new("/a"));
    s.save_state_changes(&st.take_changes()).unwrap();
    assert_eq!(s.file_state_count(), 1);

    let mut fresh = FileState::new();
    for (r, path, stat) in s.load_file_state().unwrap() {
        fresh.load(r, path, stat);
    }
    assert!(fresh.is_done(1, Path::new("/b"), a));
    assert!(!fresh.is_done(1, Path::new("/a"), a));
}

#[test]
fn forget_missing_and_prune() {
    let mut s = Store::open_in_memory().unwrap();
    let a = FileStat { size: 1, modified: 1 };
    s.save_state_changes(&[
        StateChange::Set(1, p("/exista"), a),
        StateChange::Set(1, p("/lipseste"), a),
        StateChange::Set(2, p("/lipseste"), a),
    ])
    .unwrap();
    assert_eq!(s.forget_missing(|x| x == Path::new("/exista")).unwrap(), 2);
    assert_eq!(s.file_state_count(), 1);

    let old = s.begin(NewEntry { ts: 10, ..new_entry(1, OpKind::Move) });
    s.finish(old, Status::Done);
    let open = s.begin(NewEntry { ts: 10, ..new_entry(1, OpKind::Move) });
    let fresh = s.begin(NewEntry { ts: 5000, ..new_entry(1, OpKind::Move) });
    s.finish(fresh, Status::Done);
    // O restaurare ține minte locul fișierului: rămâne, ca regula să nu-l reproceseze.
    let restored = s.begin(NewEntry { ts: 10, ..new_entry(1, OpKind::Move) });
    s.mark_undone(restored, Path::new("/dl/a.pdf"));
    let simulated = s.begin(NewEntry { ts: 10, ..new_entry(1, OpKind::Move) });
    s.finish(simulated, Status::Simulated);
    assert_eq!(s.get(simulated).unwrap().status, Status::Simulated);
    assert_eq!(s.prune_journal(1000).unwrap(), 2);
    assert!(s.get(old).is_none());
    assert!(s.get(simulated).is_none());
    assert!(s.get(restored).is_some(), "restaurările cu loc ținut minte rămân");
    assert!(s.get(open).is_some(), "intrările în curs nu se șterg");
    assert!(s.get(fresh).is_some());
}

#[test]
fn newer_schema_is_refused() {
    let path = temp_db("newer");
    drop(Store::open(&path).unwrap());
    assert!(set_user_version(&path, 99));
    assert!(matches!(Store::open(&path), Err(StoreError::NewerSchema(99))));
}

/// `user_version` din antetul fișierului SQLite e la octetul 60 (4 octeți, big-endian).
fn set_user_version(path: &Path, v: u32) -> bool {
    use std::io::{Seek, SeekFrom, Write};
    let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) else { return false };
    f.seek(SeekFrom::Start(60)).unwrap();
    f.write_all(&v.to_be_bytes()).is_ok()
}

fn invoice_rule() -> Rule {
    Rule {
        id: 1,
        name: "t".into(),
        enabled: true,
        hidden: false,
        folders: vec![WatchedFolder { path: p("/dl"), recursive: true }],
        condition: Condition::AllFiles,
        actions: vec![
            Action::Move {
                dest: Template::parse("/arhiva/{created:AAAA}").unwrap(),
                conflict: Conflict::Rename,
                keep_structure: false,
                remove_empty: false,
            },
            Action::Rename {
                name: Template::parse("{created:AAAA-LL-ZZ}_{name}").unwrap(),
                conflict: Conflict::Rename,
            },
        ],
        continue_after_move: true, simulate: false,
    }
}

/// Oprire bruscă la fiecare operație, cu jurnalul în SQLite și redeschiderea bazei.
#[test]
fn crash_recovery_with_a_real_database() {
    for n in 0..8 {
        let path = temp_db(&format!("crash{n}"));
        let mut fs = MemFs::new();
        fs.add_file("/dl/factura.pdf", 100, 5);
        let d = HashMap::new();
        {
            let mut s = Store::open(&path).unwrap();
            fs.fail_at(n);
            let rc = RenderCtx { now: 0, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &d, content: None, raw: None };
            let m = FileMeta::new("/dl/factura.pdf", "/dl", 100, timestamp(2025, 3, 7, 9, 0, 0), 5);
            let _ = run_rule(&mut fs, &mut s, &invoice_rule(), &m, &rc);
            s.simulate_crash();
        } // baza se închide ca la o oprire
        fs.clear_failure();
        let mut s = Store::open(&path).unwrap();
        recover(&mut fs, &mut s);
        let files = fs.file_paths();
        assert_eq!(files.len(), 1, "n={n}: {files:?}");
        assert_eq!(fs.stat(&files[0]).unwrap().size, 100);
        assert!(s.in_progress().is_empty());
    }
}

#[test]
fn undo_works_on_database_entries() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/factura.pdf", 100, 5);
    let d = HashMap::new();
    let mut s = Store::open_in_memory().unwrap();
    let rc = RenderCtx { now: 0, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &d, content: None, raw: None };
    let m = FileMeta::new("/dl/factura.pdf", "/dl", 100, timestamp(2025, 3, 7, 9, 0, 0), 5);
    let rep = run_rule(&mut fs, &mut s, &invoice_rule(), &m, &rc);
    assert!(rep.ok());
    let res = undo_run(&mut fs, &mut s, rep.run_id, false);
    assert!(res.iter().all(|(_, o)| matches!(o, Ok(UndoOutcome::Restored(_)))));
    assert_eq!(fs.file_paths(), vec![p("/dl/factura.pdf")]);
}

#[test]
fn restored_files_are_remembered_and_daemon_leaves_them() {
    let mut fs = MemFs::new();
    fs.add_file("/dl/factura.pdf", 100, 5);
    let d = HashMap::new();
    let mut s = Store::open_in_memory().unwrap();
    let rc = RenderCtx { now: 0, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: &d, content: None, raw: None };
    let m = FileMeta::new("/dl/factura.pdf", "/dl", 100, timestamp(2025, 3, 7, 9, 0, 0), 5);
    let rep = run_rule(&mut fs, &mut s, &invoice_rule(), &m, &rc);
    undo_run(&mut fs, &mut s, rep.run_id, false);
    assert!(s.restored_to(1, Path::new("/dl/factura.pdf"), 100, 5));
    assert!(!s.restored_to(1, Path::new("/dl/factura.pdf"), 101, 5), "alt conținut = alt fișier");
    assert!(!s.restored_to(2, Path::new("/dl/factura.pdf"), 100, 5), "altă regulă");
    // Motorul cu baza reală: fișierul restaurat rămâne pe loc.
    let mut e = org_engine::Engine::new(vec![invoice_rule()], 0);
    e.process_path(&mut fs, &mut s, &rc, Path::new("/dl/factura.pdf"), Path::new("/dl"));
    assert_eq!(fs.file_paths(), vec![p("/dl/factura.pdf")]);
}

/// O bază creată de versiunea 1 (care memora și fișierele nepotrivite) se curăță la deschidere.
#[test]
fn schema_v1_is_migrated_keeping_only_processed_files() {
    let path = temp_db("migrate");
    {
        let mut s = Store::open(&path).unwrap();
        let run = s.next_run_id();
        let id = s.begin(NewEntry { run_id: run, rule_id: 7, ts: 1, kind: OpKind::Move, src: p("/dl/a.pdf"), dst: Some(p("/pdf/a.pdf")), size: 1, mtime: 1 });
        s.finish(id, Status::Done);
        let st = FileStat { size: 1, modified: 1 };
        s.save_state_changes(&[
            StateChange::Set(7, p("/pdf/a.pdf"), st),  // procesat: rămâne
            StateChange::Set(7, p("/dl/c.txt"), st),   // nepotrivit (v1): dispare
        ])
        .unwrap();
    }
    assert!(set_user_version(&path, 1));
    let s = Store::open(&path).unwrap();
    assert_eq!(s.load_file_state().unwrap(), vec![(7, p("/pdf/a.pdf"), FileStat { size: 1, modified: 1 })]);
}

#[test]
fn processed_paths_only_for_one_rule() {
    let mut s = Store::open_in_memory().unwrap();
    let st = FileStat { size: 1, modified: 1 };
    s.save_state_changes(&[StateChange::Set(1, p("/a"), st), StateChange::Set(2, p("/b"), st), StateChange::Set(1, p("/c"), st)])
        .unwrap();
    let mut v = s.processed_paths(1).unwrap();
    v.sort();
    assert_eq!(v, vec![p("/a"), p("/c")]);
}

#[test]
fn journal_search_filters_and_pages() {
    let mut s = Store::open_in_memory().unwrap();
    let add = |s: &mut Store, rule: u64, ts: i64, src: &str, status: Status| {
        let run = s.next_run_id();
        let id = s.begin(NewEntry { run_id: run, rule_id: rule, ts, kind: OpKind::Move, src: p(src), dst: Some(p("/d/x")), size: 1, mtime: 1 });
        s.finish(id, status);
    };
    add(&mut s, 7, 100, "C:\\dl\\factura_100%.pdf", Status::Done);
    add(&mut s, 7, 200, "C:\\dl\\poza.png", Status::Failed("acces refuzat".into()));
    add(&mut s, 8, 300, "C:\\dl\\x.bak", Status::Done);
    let ids = |q: JournalQuery| { let (v, n) = s.search(&q); (v.iter().map(|e| e.id).collect::<Vec<_>>(), n) };
    let all = JournalQuery { limit: 50, ..Default::default() };
    assert_eq!(ids(all.clone()), (vec![3, 2, 1], 3));
    assert_eq!(ids(JournalQuery { rule: Some(7), ..all.clone() }), (vec![2, 1], 2));
    assert_eq!(ids(JournalQuery { status: Some("failed".into()), ..all.clone() }), (vec![2], 1));
    assert_eq!(ids(JournalQuery { since: Some(150), until: Some(350), ..all.clone() }), (vec![3, 2], 2));
    assert_eq!(ids(JournalQuery { text: Some("dl\\fact".into()), ..all.clone() }), (vec![1], 1));
    assert_eq!(ids(JournalQuery { text: Some("100%".into()), ..all.clone() }), (vec![1], 1));
    assert_eq!(ids(JournalQuery { text: Some("ACCES".into()), ..all.clone() }), (vec![2], 1));
    assert_eq!(ids(JournalQuery { limit: 2, offset: 2, ..all }), (vec![1], 3));
}

#[test]
fn pending_clears_state_and_restore_memory() {
    let mut s = Store::open_in_memory().unwrap();
    let st = FileStat { size: 1, modified: 1 };
    s.save_state_changes(&[StateChange::Set(1, p("/a"), st), StateChange::Set(1, p("/b"), st), StateChange::Set(2, p("/a"), st)]).unwrap();
    s.save_state_changes(&[StateChange::Clear(1, p("/a"))]).unwrap();
    assert_eq!(s.processed_paths(1).unwrap(), vec![p("/b")]);
    assert_eq!(s.processed_paths(2).unwrap(), vec![p("/a")], "altă regulă, neatinsă");
    s.save_state_changes(&[StateChange::ClearRule(1)]).unwrap();
    assert!(s.processed_paths(1).unwrap().is_empty());

    let run = s.next_run_id();
    let id = s.begin(NewEntry { run_id: run, rule_id: 1, ts: 1, kind: OpKind::Move, src: p("/a"), dst: Some(p("/x")), size: 5, mtime: 9 });
    s.finish(id, Status::Done);
    s.mark_undone(id, Path::new("/a"));
    assert!(s.restored_to(1, Path::new("/a"), 5, 9));
    s.forget_restored(1, Some(Path::new("/a")));
    assert!(!s.restored_to(1, Path::new("/a"), 5, 9));
    assert_eq!(s.get(id).unwrap().status, Status::Undone, "intrarea rămâne anulată");
}

#[test]
fn pending_confirmations_roundtrip_one_per_rule_and_file() {
    use org_journal::Pending;
    let mut s = Store::open_in_memory().unwrap();
    let mk = |rule: u64, path: &str, step: usize| Pending {
        id: 0,
        rule_id: rule,
        path: PathBuf::from(path),
        root: PathBuf::from("/dl"),
        size: 7,
        mtime: 3,
        step,
        ts: 100,
    };
    s.add_pending(mk(1, "/dl/a (1).pdf", 1));
    s.add_pending(mk(2, "/dl/b.pdf", 2));
    s.add_pending(mk(1, "/dl/a (1).pdf", 3));
    let p = s.pending();
    assert_eq!(p.len(), 2, "aceeași regulă și același fișier: o singură cerere");
    assert_eq!(p[0].path, PathBuf::from("/dl/a (1).pdf"));
    assert_eq!(p[0].step, 3);
    assert_eq!((p[1].root.clone(), p[1].size, p[1].mtime), (PathBuf::from("/dl"), 7, 3));
    s.remove_pending(p[0].id);
    assert_eq!(s.pending().len(), 1);
}

#[test]
fn period_stats_count_files_not_operations() {
    let mut s = Store::open_in_memory().unwrap();
    let add = |s: &mut Store, run: u64, rule: u64, ts: i64, kind: OpKind, st: Status| {
        let id = s.begin(NewEntry { run_id: run, rule_id: rule, ts, ..new_entry(run, kind) });
        s.finish(id, st);
    };
    // Rularea 1: mutare + redenumire = un singur fișier.
    add(&mut s, 1, 7, 100, OpKind::Move, Status::Done);
    add(&mut s, 1, 7, 100, OpKind::Rename, Status::Done);
    add(&mut s, 2, 7, 150, OpKind::Move, Status::Done);
    add(&mut s, 3, 8, 160, OpKind::Move, Status::Failed("x".into()));
    add(&mut s, 4, 9, 170, OpKind::Move, Status::Simulated);
    add(&mut s, 5, 8, 500, OpKind::Move, Status::Done); // în afara perioadei
    let st = s.period_stats(100, 200, &[]);
    assert_eq!(st, PeriodStats { done: 2, failed: 1, simulated: 1, undone: 0 });
    // Regulile ascunse nu se numără.
    assert_eq!(s.period_stats(100, 200, &[7]).done, 0);
    assert_eq!(s.period_stats(0, 100, &[]), PeriodStats::default());
    assert_eq!(s.top_rules(0, &[], 5), vec![(7, 2), (8, 1)]);
    assert_eq!(s.top_rules(0, &[7], 5), vec![(8, 1)]);
    assert_eq!(s.top_rules(0, &[], 1), vec![(7, 2)]);
}

#[test]
fn prints_round_trip_and_ignore_other_versions() {
    use org_engine::PrintRow;
    let mut s = Store::open_in_memory().unwrap();
    let a = (PathBuf::from("C:/dl/a.bin"), PrintRow { size: 10, modified: 5, quick: Some(u64::MAX - 3), full: None });
    let b = (PathBuf::from("C:/dl/b.bin"), PrintRow { size: 1 << 40, modified: -7, quick: Some(1), full: Some(2) });
    s.save_prints(&[a.clone(), b.clone()]).unwrap();
    let mut got = s.load_prints().unwrap();
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got, vec![a.clone(), b.clone()]);
    // Înlocuire.
    let a2 = (a.0.clone(), PrintRow { full: Some(9), ..a.1 });
    s.save_prints(&[a2.clone()]).unwrap();
    assert!(s.load_prints().unwrap().contains(&a2));
    s.delete_prints(&[b.0.clone()]).unwrap();
    assert_eq!(s.load_prints().unwrap(), vec![a2]);
}

#[test]
fn verdicts_round_trip_and_go_away_with_the_file() {
    use org_engine::{PrintRow, VerdictRow};
    let mut s = Store::open_in_memory().unwrap();
    let a = PathBuf::from("C:/dl/factura.pdf");
    let rows = vec![
        ((a.clone(), u64::MAX - 1), VerdictRow { size: 10, modified: 5, matched: true }),
        ((a.clone(), 7), VerdictRow { size: 10, modified: 5, matched: false }),
        ((PathBuf::from("C:/dl/b.pdf"), 7), VerdictRow { size: 3, modified: 1, matched: true }),
    ];
    s.save_verdicts(&rows).unwrap();
    let mut got = s.load_verdicts().unwrap();
    got.sort_by(|x, y| x.0.cmp(&y.0));
    let mut want = rows.clone();
    want.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got, want);
    s.save_prints(&[(a.clone(), PrintRow { size: 10, modified: 5, quick: Some(1), full: None })]).unwrap();
    // Fișierul dispărut: tot ce se știa despre el pleacă.
    s.delete_prints(&[a]).unwrap();
    assert!(s.load_prints().unwrap().is_empty());
    assert_eq!(s.load_verdicts().unwrap().len(), 1);
}
