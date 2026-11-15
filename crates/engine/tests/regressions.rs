// Regresii găsite la revizia din 11.10.2026: fiecare test descrie comportamentul corect.
use std::collections::HashMap;
use std::path::PathBuf;
use org_core::timefmt::Lang;
use org_core::*;
use org_engine::*;
use org_fs::MemFs;
use org_journal::{undo, MemJournal};

fn p(s: &str) -> PathBuf { PathBuf::from(s) }
fn ctx(d: &HashMap<String, Dictionary>, now: i64) -> RenderCtx<'_> { RenderCtx { now, tz: Tz::Fixed(0), lang: Lang::Ro, dicts: d, content: None } }
const T0: i64 = 1_800_000_000;
const DAY: i64 = 86_400;

fn rule(cond: Condition, dest: &str) -> Rule {
    Rule { id: 1, name: "r".into(), enabled: true, hidden: false,
        folders: vec![WatchedFolder { path: p("/dl"), recursive: false }],
        condition: Condition::All(vec![cond]),
        actions: vec![Action::Move { dest: Template::parse(dest).unwrap(), conflict: Conflict::Rename, keep_structure: false, remove_empty: false }],
        continue_after_move: false, simulate: false }
}
fn ext(v: &str) -> Condition { Condition::Text { field: TextField::Extension, op: TextOp::Is, value: v.into() } }

#[test]
fn restore_sticks() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut e = Engine::new(vec![rule(ext("pdf"), "/pdf")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.stat("/pdf/a.pdf").is_some());
    undo(&mut fs, &mut j, 1, false).unwrap();            // utilizatorul restaurează
    assert!(fs.stat("/dl/a.pdf").is_some());
    e.process_path(&mut fs, &mut j, &ctx(&d, T0 + 5), &p("/dl/a.pdf"), &p("/dl")); // evenimentul watcher-ului
    assert!(fs.stat("/dl/a.pdf").is_some(), "restaurarea a fost anulată: daemonul a mutat fișierul din nou");
}

#[test]
fn older_than_fires_when_file_ages() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/old.bin", 10, T0 - 10 * DAY);    // 10 zile
    let c = Condition::Date { field: DateField::Modified, op: DateOp::OlderThanDays(30) };
    let mut e = Engine::new(vec![rule(c, "/arhiva")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 25 * DAY));  // acum are 35 de zile
    assert!(fs.stat("/arhiva/old.bin").is_some(), "fișierul a devenit mai vechi de 30 de zile, dar nu a fost procesat");
}

#[test]
fn edited_rule_applies_to_existing_files() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.docx", 10, 1);
    let mut e = Engine::new(vec![rule(ext("pdf"), "/doc")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    e.set_rules(vec![rule(Condition::Any(vec![ext("pdf"), ext("docx")]), "/doc")]); // utilizatorul adaugă docx
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 60));
    assert!(fs.stat("/doc/a.docx").is_some(), "regula editată nu se aplică fișierelor existente");
}

#[test]
fn failed_move_is_retried() {
    use org_journal::{Journal, Status};
    // Caută pasul la care eroarea injectată lovește chiar mutarea (intrarea din jurnal = Failed).
    for k in 0..6 {
        let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
        fs.add_file("/dl/a.pdf", 10, 1);
        let mut e = Engine::new(vec![rule(ext("pdf"), "/pdf")], 0);
        fs.fail_at(fs.ops() + k);
        e.scan(&mut fs, &mut j, &ctx(&d, T0));
        if !matches!(j.get(1).map(|x| x.status), Some(Status::Failed(_))) { continue; }
        fs.clear_failure();
        e.scan(&mut fs, &mut j, &ctx(&d, T0 + 60));
        assert!(fs.stat("/pdf/a.pdf").is_some(), "după un eșec temporar (pas {k}), fișierul nu mai e reîncercat");
        return;
    }
    panic!("nu am reușit să injectez eroarea");
}

#[test]
fn extension_with_dot_matches() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut e = Engine::new(vec![rule(ext(".pdf"), "/pdf")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.stat("/pdf/a.pdf").is_some(), "extensia scrisă cu punct (.pdf) nu se potrivește");
}


#[test]
fn relative_destination_is_inside_watched_folder() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut e = Engine::new(vec![rule(ext("pdf"), "PDF/{modified:AAAA}")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert!(fs.stat("/dl/PDF/1970/a.pdf").is_some(), "{:?}", fs.file_paths());
}

#[test]
fn program_files_are_never_touched() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/organizator.db", 10, 1);
    fs.add_file("/dl/organizator.db-wal", 10, 1);
    fs.add_file("/dl/alt.db", 10, 1);
    let mut e = Engine::new(vec![rule(ext("db"), "/x")], 0);
    e.exclude_prefixes(&[std::path::Path::new("/DL/Organizator.db")]);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let mut f = fs.file_paths();
    f.sort();
    assert_eq!(f, vec![p("/dl/organizator.db"), p("/dl/organizator.db-wal"), p("/x/alt.db")]);
}

#[test]
fn gives_up_after_last_backoff_and_retries_are_scheduled() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let r = rule(ext("pdf"), "/pdf"); // eroarea se injectează înainte de fiecare încercare
    let mut e = Engine::new(vec![r], 0);
    let mut now = T0;
    fs.fail_at(fs.ops());
    e.scan(&mut fs, &mut j, &ctx(&d, now));
    let mut attempts = 1;
    while let Some(t) = e.next_retry_at() {
        assert!(t > now);
        now = t;
        assert_eq!(e.take_due_retries(now), vec![p("/dl/a.pdf")]);
        fs.fail_at(fs.ops());
        e.process_path(&mut fs, &mut j, &ctx(&d, now), &p("/dl/a.pdf"), &p("/dl"));
        attempts += 1;
        assert!(attempts <= 10, "reîncercări fără sfârșit");
    }
    assert_eq!(attempts, 5, "1 încercare + 4 reîncercări");
    // după renunțare: nu mai încearcă la scanări
    let before = j.entries().len();
    e.scan(&mut fs, &mut j, &ctx(&d, now + 10 * 3600));
    assert_eq!(j.entries().len(), before);
    assert!(fs.stat("/dl/a.pdf").is_some());
}

#[test]
fn restored_then_modified_file_is_processed_again() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut e = Engine::new(vec![rule(ext("pdf"), "/pdf")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    undo(&mut fs, &mut j, 1, false).unwrap();
    e.process_path(&mut fs, &mut j, &ctx(&d, T0 + 5), &p("/dl/a.pdf"), &p("/dl"));
    assert!(fs.stat("/dl/a.pdf").is_some());
    fs.add_file("/dl/a.pdf", 20, 50); // utilizatorul îl modifică
    e.process_path(&mut fs, &mut j, &ctx(&d, T0 + 10), &p("/dl/a.pdf"), &p("/dl"));
    assert!(fs.stat("/pdf/a.pdf").is_some(), "un fișier restaurat și apoi modificat trebuie procesat din nou");
}

#[test]
fn time_relative_rules_are_detected() {
    let older = rule(Condition::Date { field: DateField::Created, op: DateOp::OlderThanDays(7) }, "/x");
    let fixed = rule(Condition::Date { field: DateField::Created, op: DateOp::Before(T0) }, "/x");
    assert!(Engine::new(vec![older.clone()], 0).needs_periodic_scan());
    assert!(!Engine::new(vec![fixed], 0).needs_periodic_scan());
    let mut off = older;
    off.enabled = false;
    assert!(!Engine::new(vec![off], 0).needs_periodic_scan());
}

#[test]
fn overwrite_sends_old_file_to_trash_and_run_undo_restores_both() {
    use org_journal::undo_run;
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    fs.add_file("/pdf/a.pdf", 99, 7); // versiunea veche
    let mut r = rule(ext("pdf"), "/pdf");
    r.actions = vec![Action::Move { dest: Template::parse("/pdf").unwrap(), conflict: Conflict::Overwrite, keep_structure: false, remove_empty: false }];
    let mut e = Engine::new(vec![r], 0);
    let rep = e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(fs.stat("/pdf/a.pdf").unwrap().size, 10);
    assert_eq!(fs.trashed(), vec![p("/pdf/a.pdf")], "versiunea veche e în Coș, nu ștearsă");
    let run = rep.processed[0].run_id;
    let res = undo_run(&mut fs, &mut j, run, false);
    assert!(res.iter().all(|(_, o)| matches!(o, Ok(org_journal::UndoOutcome::Restored(_)))), "{res:?}");
    assert_eq!(fs.stat("/dl/a.pdf").unwrap().size, 10);
    assert_eq!(fs.stat("/pdf/a.pdf").unwrap().size, 99, "versiunea veche a revenit");
}

#[test]
fn undoing_a_copy_sends_the_copy_to_trash() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut r = rule(ext("pdf"), "/bk");
    r.actions = vec![Action::Copy { dest: Template::parse("/bk").unwrap(), conflict: Conflict::Rename, keep_structure: false, then_target: false }];
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    undo(&mut fs, &mut j, 1, false).unwrap();
    assert!(fs.stat("/bk/a.pdf").is_none());
    assert_eq!(fs.trashed(), vec![p("/bk/a.pdf")]);
}

#[test]
fn failed_command_is_not_retried() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    fs.run_exit = 1;
    let mut r = rule(ext("pdf"), "/x");
    r.actions = vec![Action::Run { program: Template::parse("unealta").unwrap(), args: vec![] }];
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    assert_eq!(e.next_retry_at(), None);
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 7200));
    assert_eq!(fs.ran.len(), 1, "comanda a rulat o singură dată");
}

/// Ca pe Windows: listarea dă metadatele (aici, configurabil învechite); numărăm citirile.
struct ListedFs {
    inner: MemFs,
    stale: HashMap<PathBuf, org_fs::FileStat>,
    reads: std::cell::Cell<usize>,
}

impl org_fs::Fs for ListedFs {
    fn list_dir(&self, dir: &std::path::Path) -> std::io::Result<Vec<org_fs::DirEntry>> {
        let mut v = self.inner.list_dir(dir)?;
        for e in &mut v {
            if !e.is_dir {
                let s = self.stale.get(&e.path).copied().or_else(|| self.inner.stat(&e.path)).unwrap();
                e.stat = Some((s, s.modified));
            }
        }
        Ok(v)
    }
    fn metadata(&self, p: &std::path::Path) -> std::io::Result<org_fs::FileStat> {
        self.reads.set(self.reads.get() + 1);
        self.inner.metadata(p)
    }
    fn exists(&self, p: &std::path::Path) -> bool { self.inner.exists(p) }
    fn read_prefix(&self, p: &std::path::Path, m: u64) -> std::io::Result<Vec<u8>> { self.inner.read_prefix(p, m) }
    fn in_use(&self, p: &std::path::Path) -> bool { self.inner.in_use(p) }
    fn create_dir_all(&mut self, p: &std::path::Path) -> std::io::Result<()> { self.inner.create_dir_all(p) }
    fn rename(&mut self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> { self.inner.rename(a, b) }
    fn copy(&mut self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<u64> { self.inner.copy(a, b) }
    fn remove_file(&mut self, p: &std::path::Path) -> std::io::Result<()> { self.inner.remove_file(p) }
    fn trash(&mut self, p: &std::path::Path) -> std::io::Result<()> { self.inner.trash(p) }
    fn restore_from_trash(&mut self, p: &std::path::Path) -> std::io::Result<()> { self.inner.restore_from_trash(p) }
    fn remove_dir_if_empty(&mut self, p: &std::path::Path) -> std::io::Result<bool> { self.inner.remove_dir_if_empty(p) }
    fn run(&mut self, p: &str, a: &[String]) -> std::io::Result<i32> { self.inner.run(p, a) }
    fn run_shell(&mut self, l: &str) -> std::io::Result<i32> { self.inner.run_shell(l) }
}

#[test]
fn scan_uses_listed_metadata_and_rereads_only_before_acting() {
    let mut inner = MemFs::new();
    for i in 0..200 {
        inner.add_file(format!("/dl/f{i}.txt"), 10, 1);
    }
    inner.add_file("/dl/mare.pdf", 7, 1); // de fapt 7 octeți...
    inner.add_file("/dl/ok.pdf", 50, 1);
    let mut stale = HashMap::new();
    stale.insert(p("/dl/mare.pdf"), org_fs::FileStat { size: 9, modified: 1 }); // ...dar listarea zice 9
    let mut fs = ListedFs { inner, stale, reads: std::cell::Cell::new(0) };
    let (mut j, d) = (MemJournal::new(), HashMap::new());
    let big_pdf = Condition::All(vec![ext("pdf"), Condition::Size { op: SizeOp::Larger, bytes: 8 }]);
    let mut e = Engine::new(vec![rule(big_pdf, "/pdf")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    let mut moved: Vec<PathBuf> = fs.inner.file_paths().into_iter().filter(|x| x.starts_with("/pdf")).collect();
    moved.sort();
    assert_eq!(moved, vec![p("/pdf/ok.pdf")], "valorile proaspete decid, nu cele din listare");
    assert!(fs.reads.get() < 20, "{} citiri pentru 202 fișiere: fișierele nepotrivite nu se mai citesc", fs.reads.get());
}

#[test]
fn mark_pending_reprocesses_file_and_beats_restore() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut r = rule(ext("pdf"), "/bk");
    r.actions = vec![Action::Copy { dest: Template::parse("/bk").unwrap(), conflict: Conflict::Rename, keep_structure: false, then_target: false }];
    r.continue_after_move = true;
    let mut e = Engine::new(vec![r], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 10));
    assert!(fs.stat("/bk/a_2.pdf").is_none(), "procesat o dată");
    // în așteptare: copiat din nou
    e.mark_pending(&mut j, 1, Some(std::path::Path::new("/dl/a.pdf")));
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 20));
    assert!(fs.stat("/bk/a_2.pdf").is_some(), "reprocesat după „în așteptare”");
    assert!(e.state.take_changes().iter().any(|c| matches!(c, StateChange::Clear(1, _))));
}

#[test]
fn mark_pending_after_restore_lets_rule_run_again() {
    let (mut fs, mut j, d) = (MemFs::new(), MemJournal::new(), HashMap::new());
    fs.add_file("/dl/a.pdf", 10, 1);
    let mut e = Engine::new(vec![rule(ext("pdf"), "/pdf")], 0);
    e.scan(&mut fs, &mut j, &ctx(&d, T0));
    undo(&mut fs, &mut j, 1, false).unwrap();
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 5));
    assert!(fs.stat("/dl/a.pdf").is_some(), "restaurarea rămâne");
    e.mark_pending(&mut j, 1, None); // toată regula
    e.scan(&mut fs, &mut j, &ctx(&d, T0 + 10));
    assert!(fs.stat("/pdf/a.pdf").is_some(), "„în așteptare” bate restaurarea");
}
