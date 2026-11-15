use std::collections::HashSet;
use std::path::{Path, PathBuf};

use org_fs::{Fs, MemFs};
use org_journal::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn entry(kind: OpKind, src: &str, dst: Option<&str>, size: u64, mtime: i64) -> NewEntry {
    NewEntry { run_id: 1, rule_id: 7, ts: 0, kind, src: p(src), dst: dst.map(p), size, mtime }
}

// ---------- recuperare ----------

#[test]
fn recover_move_that_never_started() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.add_dir("/b");
    let mut j = MemJournal::new();
    let id = j.begin(entry(OpKind::Move, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    let r = recover(&mut fs, &mut j);
    assert!(matches!(&r[..], [(i, Status::Failed(_))] if *i == id));
    assert!(fs.stat("/a/x.pdf").is_some(), "sursa rămâne");
    assert!(j.in_progress().is_empty());
}

#[test]
fn recover_move_that_finished_but_was_not_closed() {
    let mut fs = MemFs::new();
    fs.add_file("/b/x.pdf", 10, 5);
    let mut j = MemJournal::new();
    j.begin(entry(OpKind::Move, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    let r = recover(&mut fs, &mut j);
    assert_eq!(r[0].1, Status::Done);
}

#[test]
fn recover_cross_volume_move_with_complete_copy_finishes_the_delete() {
    let mut fs = MemFs::new();
    fs.add_file("/c/x.pdf", 10, 5);
    fs.add_file("/d/x.pdf", 10, 5);
    let mut j = MemJournal::new();
    j.begin(entry(OpKind::Move, "/c/x.pdf", Some("/d/x.pdf"), 10, 5));
    let r = recover(&mut fs, &mut j);
    assert_eq!(r[0].1, Status::Done);
    assert!(fs.stat("/c/x.pdf").is_none());
    assert_eq!(fs.stat("/d/x.pdf").unwrap().size, 10);
}

#[test]
fn recover_cross_volume_move_with_partial_copy_keeps_the_source() {
    let mut fs = MemFs::new();
    fs.add_file("/c/x.pdf", 10, 5);
    fs.add_file("/d/x.pdf", 5, 5); // copie incompletă
    let mut j = MemJournal::new();
    j.begin(entry(OpKind::Move, "/c/x.pdf", Some("/d/x.pdf"), 10, 5));
    let r = recover(&mut fs, &mut j);
    assert!(matches!(r[0].1, Status::Failed(_)));
    assert_eq!(fs.stat("/c/x.pdf").unwrap().size, 10, "sursa intactă");
    assert!(fs.stat("/d/x.pdf").is_none(), "copia incompletă dispare");
}

#[test]
fn recover_copy() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.add_file("/b/x.pdf", 10, 5);
    fs.add_file("/a/y.pdf", 10, 5);
    fs.add_file("/b/y.pdf", 3, 5); // incompletă
    let mut j = MemJournal::new();
    j.begin(entry(OpKind::Copy, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    j.begin(entry(OpKind::Copy, "/a/y.pdf", Some("/b/y.pdf"), 10, 5));
    j.begin(entry(OpKind::Copy, "/a/z.pdf", Some("/b/z.pdf"), 10, 5)); // nu a început
    let r = recover(&mut fs, &mut j);
    assert_eq!(r[0].1, Status::Done);
    assert!(matches!(r[1].1, Status::Failed(_)));
    assert!(matches!(r[2].1, Status::Failed(_)));
    assert!(fs.stat("/b/y.pdf").is_none());
    assert!(fs.stat("/b/x.pdf").is_some());
}

#[test]
fn recover_trash_delete_and_run() {
    let mut fs = MemFs::new();
    fs.add_file("/a/still.pdf", 10, 5);
    let mut j = MemJournal::new();
    j.begin(entry(OpKind::Trash, "/a/gone.pdf", None, 10, 5));
    j.begin(entry(OpKind::Delete, "/a/still.pdf", None, 10, 5));
    j.begin(entry(OpKind::Run, "/a/gone.pdf", None, 10, 5));
    let r = recover(&mut fs, &mut j);
    assert_eq!(r[0].1, Status::Done, "fișierul a dispărut: operația s-a terminat");
    assert!(matches!(r[1].1, Status::Failed(_)), "fișierul încă există: nu s-a șters");
    assert!(matches!(r[2].1, Status::Failed(_)), "o comandă nu se reia");
    assert!(fs.stat("/a/still.pdf").is_some());
}

#[test]
fn recover_leaves_closed_entries_alone() {
    let mut fs = MemFs::new();
    let mut j = MemJournal::new();
    let id = j.begin(entry(OpKind::Move, "/a/x", Some("/b/x"), 1, 1));
    j.finish(id, Status::Done);
    assert!(recover(&mut fs, &mut j).is_empty());
    assert_eq!(j.get(id).unwrap().status, Status::Done);
}

// ---------- restaurare ----------

/// Un fișier mutat din /a în /b, cu intrarea de jurnal închisă `Done`.
fn moved() -> (MemFs, MemJournal, u64) {
    let mut fs = MemFs::new();
    fs.add_file("/b/x.pdf", 10, 5);
    let mut j = MemJournal::new();
    let id = j.begin(entry(OpKind::Move, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    j.finish(id, Status::Done);
    (fs, j, id)
}

#[test]
fn undo_move_restores_and_recreates_the_old_folder() {
    let (mut fs, mut j, id) = moved();
    assert!(!fs.has_dir("/a"));
    let out = undo(&mut fs, &mut j, id, false).unwrap();
    assert_eq!(out, UndoOutcome::Restored(p("/a/x.pdf")));
    assert!(fs.stat("/a/x.pdf").is_some() && fs.stat("/b/x.pdf").is_none());
    assert_eq!(j.get(id).unwrap().status, Status::Undone);
    // a doua oară nu mai face nimic
    assert_eq!(
        undo(&mut fs, &mut j, id, false).unwrap(),
        UndoOutcome::Unrecoverable("deja anulată")
    );
}

#[test]
fn undo_never_overwrites_an_occupied_spot() {
    let (mut fs, mut j, id) = moved();
    fs.add_file("/a/x.pdf", 99, 1); // altceva a ocupat locul vechi
    let out = undo(&mut fs, &mut j, id, false).unwrap();
    assert_eq!(out, UndoOutcome::Restored(p("/a/x_2.pdf")));
    assert_eq!(fs.stat("/a/x.pdf").unwrap().size, 99, "fișierul străin rămâne neatins");
    assert_eq!(fs.stat("/a/x_2.pdf").unwrap().size, 10);
}

#[test]
fn undo_asks_before_touching_a_modified_file() {
    let (mut fs, mut j, id) = moved();
    fs.add_file("/b/x.pdf", 10, 6); // modificat după mutare
    let out = undo(&mut fs, &mut j, id, false).unwrap();
    assert!(matches!(out, UndoOutcome::NeedsConfirmation(_)));
    assert!(fs.stat("/b/x.pdf").is_some(), "nimic nu s-a mutat");
    assert_eq!(j.get(id).unwrap().status, Status::Done);
    // cu confirmare, se restaurează
    let out = undo(&mut fs, &mut j, id, true).unwrap();
    assert_eq!(out, UndoOutcome::Restored(p("/a/x.pdf")));
}

#[test]
fn undo_reports_a_missing_file() {
    let (mut fs, mut j, id) = moved();
    fs.remove_file(Path::new("/b/x.pdf")).unwrap();
    assert!(matches!(undo(&mut fs, &mut j, id, false).unwrap(), UndoOutcome::Unrecoverable(_)));
}

#[test]
fn undo_copy_removes_only_an_unchanged_copy() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.add_file("/b/x.pdf", 10, 5);
    let mut j = MemJournal::new();
    let id = j.begin(entry(OpKind::Copy, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    j.finish(id, Status::Done);
    assert_eq!(undo(&mut fs, &mut j, id, false).unwrap(), UndoOutcome::Restored(p("/a/x.pdf")));
    assert!(fs.stat("/b/x.pdf").is_none());
    assert!(fs.stat("/a/x.pdf").is_some(), "originalul rămâne");

    fs.add_file("/b/y.pdf", 11, 9); // copie modificată
    let id2 = j.begin(entry(OpKind::Copy, "/a/x.pdf", Some("/b/y.pdf"), 10, 5));
    j.finish(id2, Status::Done);
    assert!(matches!(undo(&mut fs, &mut j, id2, false).unwrap(), UndoOutcome::NeedsConfirmation(_)));
    assert!(fs.stat("/b/y.pdf").is_some());
}

#[test]
fn undo_trash_delete_and_run() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.trash(Path::new("/a/x.pdf")).unwrap();
    let mut j = MemJournal::new();
    let t = j.begin(entry(OpKind::Trash, "/a/x.pdf", None, 10, 5));
    let d = j.begin(entry(OpKind::Delete, "/a/d.pdf", None, 10, 5));
    let r = j.begin(entry(OpKind::Run, "/a/x.pdf", None, 10, 5));
    for id in [t, d, r] {
        j.finish(id, Status::Done);
    }
    assert_eq!(undo(&mut fs, &mut j, t, false).unwrap(), UndoOutcome::Restored(p("/a/x.pdf")));
    assert!(fs.stat("/a/x.pdf").is_some());
    assert!(matches!(undo(&mut fs, &mut j, d, false).unwrap(), UndoOutcome::Unrecoverable(_)));
    assert_eq!(undo(&mut fs, &mut j, r, false).unwrap(), UndoOutcome::NotUndoable);
}

#[test]
fn undo_refuses_unfinished_entries() {
    let mut fs = MemFs::new();
    let mut j = MemJournal::new();
    let id = j.begin(entry(OpKind::Move, "/a/x", Some("/b/x"), 1, 1));
    assert!(matches!(undo(&mut fs, &mut j, id, false).unwrap(), UndoOutcome::Unrecoverable(_)));
    assert!(matches!(undo(&mut fs, &mut j, 999, false).unwrap(), UndoOutcome::Unrecoverable(_)));
}

#[test]
fn undo_run_reverses_a_chain_in_the_right_order() {
    // /a/x.pdf -> /b/x.pdf (mutare), apoi /b/x.pdf -> /b/2026_x.pdf (redenumire)
    let mut fs = MemFs::new();
    fs.add_file("/b/2026_x.pdf", 10, 5);
    let mut j = MemJournal::new();
    let m = j.begin(entry(OpKind::Move, "/a/x.pdf", Some("/b/x.pdf"), 10, 5));
    let r = j.begin(entry(OpKind::Rename, "/b/x.pdf", Some("/b/2026_x.pdf"), 10, 5));
    j.finish(m, Status::Done);
    j.finish(r, Status::Done);
    let results = undo_run(&mut fs, &mut j, 1, false);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, r, "întâi se anulează ultima operație");
    assert!(results.iter().all(|(_, o)| matches!(o, Ok(UndoOutcome::Restored(_)))));
    assert_eq!(fs.file_paths(), vec![p("/a/x.pdf")]);
}

// ---------- reguli ascunse ----------

#[test]
fn hidden_rules_entries_are_filtered_until_unlocked() {
    let mut j = MemJournal::new();
    for rule_id in [1, 2, 2, 3] {
        let mut e = entry(OpKind::Move, "/a/x", Some("/b/x"), 1, 1);
        e.rule_id = rule_id;
        j.begin(e);
    }
    let hidden: HashSet<u64> = HashSet::from([2]);
    let locked: Vec<u64> = visible_entries(j.entries(), &hidden, false).map(|e| e.rule_id).collect();
    let open: Vec<u64> = visible_entries(j.entries(), &hidden, true).map(|e| e.rule_id).collect();
    assert_eq!(locked, vec![1, 3]);
    assert_eq!(open, vec![1, 2, 2, 3]);
}

#[test]
fn run_ids_are_sequential() {
    let mut j = MemJournal::new();
    assert_eq!((j.next_run_id(), j.next_run_id()), (1, 2));
    let _ = Fs::exists(&MemFs::new(), Path::new("/")); // folosește trait-ul importat
}
