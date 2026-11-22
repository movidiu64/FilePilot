//! The FilePilot daemon: loads the rules from a `.orgrules` file, scans the watched folders
//! and then listens for changes (ReadDirectoryChangesW through `notify`).
//!
//! Low footprint: with nothing to do, the thread sleeps blocked on the channel (60 s) and uses
//! no CPU. Events are gathered for 500 ms (debounce); files still being written are retried
//! every second until they are stable.
//!
//! Commands: `filepilot journal <db> [--limit N]`, `filepilot undo <db> <id> | --run <run> [--force]`
//! (`jurnal` is accepted as well).
//!
//! Usage: `filepilot [rules.orgrules] [--db file.db] [--once] [--settle SECONDS] [--lang ro|en]`
//! Without a rules file it uses `%APPDATA%\FilePilot` and reloads the rules when they change.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use org_core::timefmt::{format as fmt_time, Lang};
use org_core::{Dictionary, RenderCtx, Rule, Tz};
use org_engine::{Engine, Outcome};
use org_paths::{
    acquire_lock, is_request_file, pending_requests, DataFiles, BACKUP_DIR, PAUSE_FILE, SETTINGS_FILE, STATUS_FILE,
};
use org_portable::json::Value;
use org_portable::request::{parse_request, Request};
use org_sysfs::SysFs;
use org_journal::{recover, undo, undo_run, Entry, Journal, OpKind, Status, UndoOutcome};
use org_store::Store;

const DEBOUNCE: Duration = Duration::from_millis(500);
const IDLE: Duration = Duration::from_secs(60);
const BUSY: Duration = Duration::from_millis(250);
const RETRY_ROOTS: Duration = Duration::from_secs(30);
const RESCAN: Duration = Duration::from_secs(1);
/// Cât se așteaptă până se reîncearcă un fișier deschis de alt program.
const IN_USE: Duration = Duration::from_secs(org_engine::IN_USE_RECHECK as u64);
/// Cât de des se rescanează folderele când există reguli „mai vechi de N zile” /
/// „în ultimele N zile”: fișierele neschimbate pot începe să se potrivească.
const PERIODIC: Duration = Duration::from_secs(3600);

struct Opts {
    /// `None` = folderul de date implicit.
    rules: Option<PathBuf>,
    db: Option<PathBuf>,
    once: bool,
    /// `--settle` din linia de comandă; altfel se ia (și se urmărește) din Setări.
    settle: Option<i64>,
    lang: Lang,
}

fn parse_args() -> Result<Opts, String> {
    let mut rules = None;
    let mut db = None;
    let mut once = false;
    let mut settle = None;
    let mut lang = saved_lang();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--once" => once = true,
            "--no-tray" => {}
            "--settle" => {
                settle = Some(it.next().and_then(|v| v.parse().ok()).ok_or("--settle cere un număr de secunde")?)
            }
            "--db" => db = Some(PathBuf::from(it.next().ok_or("--db cere o cale de fișier")?)),
            "--lang" => {
                lang = match it.next().as_deref() {
                    Some("ro") => Lang::Ro,
                    Some("en") => Lang::En,
                    _ => return Err("--lang cere ro sau en".into()),
                }
            }
            x if x.starts_with("--") => return Err(format!("opțiune necunoscută: {x}")),
            x => rules = Some(PathBuf::from(x)),
        }
    }
    Ok(Opts { rules, db, once, settle, lang })
}

/// O valoare din `settings.json` (scris de fereastră).
fn saved_setting(key: &str) -> Option<Value> {
    let path = org_paths::data_dir().join(org_paths::SETTINGS_FILE);
    let t = std::fs::read_to_string(path).ok()?;
    match org_portable::json::parse(&t) {
        Ok(Value::Obj(o)) => o.into_iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// Limba aleasă în fereastră; implicit română.
fn saved_lang() -> Lang {
    match saved_setting("lang") {
        Some(Value::Str(l)) if l == "en" => Lang::En,
        _ => Lang::Ro,
    }
}

const DEFAULT_SETTLE: i64 = 5;

/// Câte zile se păstrează jurnalul (Setări); 0 = tot. Implicit 90.
fn saved_journal_days() -> i64 {
    match saved_setting("journal_days") {
        Some(Value::Int(n)) => n.clamp(0, 36_500),
        _ => 90,
    }
}

fn prune(journal: &mut Store) {
    let days = saved_journal_days();
    if days == 0 {
        return;
    }
    match journal.prune_journal(now_ts() - days * 86_400) {
        Ok(0) => {}
        Ok(n) => println!("[jurnal] șterse {n} intrări mai vechi de {days} zile"),
        Err(e) => eprintln!("[jurnal] curățenie: {e}"),
    }
}

/// Cât stă un fișier nemodificat înainte de procesare (Setări); implicit 5 s.
fn saved_settle() -> i64 {
    match saved_setting("settle_secs") {
        Some(Value::Int(n)) => n.clamp(0, 86_400),
        _ => DEFAULT_SETTLE,
    }
}

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Decalajul fusului orar local, în secunde (ține cont de ora de vară).
fn local_offset() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64
}

/// Decalajul local la un moment dat (ora de vară după data respectivă).
fn offset_at(ts: i64) -> i64 {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map_or_else(local_offset, |d| d.offset().local_minus_utc() as i64)
}

fn clock(lang: Lang) -> String {
    fmt_time(now_ts(), local_offset(), "AAAA-LL-ZZ HH:MM:SS", lang)
}

/// Folderul monitorizat cel mai specific care conține calea.
fn root_for<'a>(roots: &'a [(PathBuf, bool)], path: &Path) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|(r, _)| path.starts_with(r))
        .max_by_key(|(r, _)| r.components().count())
        .map(|(r, _)| r.as_path())
}

/// Ce vede fereastra (și pictograma din tray) despre daemon: `daemon-status.json`.
#[derive(Debug, Clone, Default, PartialEq)]
struct DaemonStatus {
    started: i64,
    rules: usize,
    active: usize,
    folders: Vec<String>,
    unavailable: Vec<String>,
    /// `Some(i64::MAX)` = pauză până la reluare.
    paused_until: Option<i64>,
    /// Ziua locală (AAAA-LL-ZZ) pentru numărătorile de mai jos.
    day: String,
    done: u64,
    failed: u64,
    last_error: Option<String>,
    /// Confirmări care așteaptă răspuns în fereastră.
    pending: usize,
    /// Scanarea completă în curs: (fișiere verificate, total, începută la).
    scan: Option<(usize, usize, i64)>,
    /// Where the latest files went, newest first (for the tray menu; not in the JSON).
    recent: Vec<PathBuf>,
    /// A newer FilePilot found on GitHub (for the tray menu; the window reads update.json).
    update: Option<String>,
}

/// How often the daemon looks whether a check for updates is due (it is, once a day).
const UPDATE_LOOK: Duration = Duration::from_secs(3600);
/// The first look after starting: not while the initial scan is busy.
const UPDATE_FIRST_LOOK: Duration = Duration::from_secs(120);
static UPDATE_CHECKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The user agreed to updates being checked (Settings, or the question on first start).
fn updates_on() -> bool {
    matches!(saved_setting("updates"), Some(Value::Str(s)) if s == "on")
}

/// Asks GitHub for the latest release, in the background, if a check is due. The result goes
/// to update.json, which the window reads; its change brings the tray menu up to date.
fn check_updates_if_due(dir: &Path) {
    let path = dir.join(org_update::STATE_FILE);
    if !updates_on() || !org_update::read_state(&path).due(now_ts()) {
        return;
    }
    if UPDATE_CHECKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let st = org_update::check_and_record(&path, now_ts());
        match (&st.error, st.available()) {
            (Some(e), _) => println!("[actualizare] verificarea a eșuat: {e}"),
            (None, Some(v)) => println!("[actualizare] versiune nouă: {v}"),
            (None, None) => println!("[actualizare] FilePilot e la zi"),
        }
        UPDATE_CHECKING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// How many files the tray menu lists, and how many journal entries are looked at for them.
const RECENT_SHOWN: usize = 10;
const RECENT_SCAN: u32 = 200;

/// Where the files most recently moved, renamed, copied or extracted ended up, newest first,
/// each place once. A place a later operation moved the file away from is left out (moved,
/// then renamed: only the final name), and so are the files of hidden rules.
fn recent_places(entries: &[Entry], hidden: &HashSet<u64>, n: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut left: Vec<&Path> = Vec::new();
    for e in entries {
        if out.len() >= n {
            break;
        }
        if e.status != Status::Done || hidden.contains(&e.rule_id) {
            continue;
        }
        let moving = matches!(e.kind, OpKind::Move | OpKind::Rename | OpKind::ChangeExt | OpKind::Trash | OpKind::Delete);
        let placed = matches!(e.kind, OpKind::Move | OpKind::Copy | OpKind::Rename | OpKind::ChangeExt | OpKind::Extract);
        if let (true, Some(d)) = (placed, &e.dst) {
            if !left.contains(&d.as_path()) && !out.contains(d) {
                out.push(d.clone());
            }
        }
        if moving {
            left.push(&e.src);
        }
    }
    out
}

impl DaemonStatus {
    fn to_json(&self) -> String {
        let strs = |v: &[String]| Value::Arr(v.iter().map(|x| Value::Str(x.clone())).collect());
        let opt_int = |v: Option<i64>| v.map_or(Value::Null, Value::Int);
        org_portable::json::to_string_pretty(&Value::Obj(vec![
            ("pid".into(), Value::Int(std::process::id() as i64)),
            ("started".into(), Value::Int(self.started)),
            ("updated".into(), Value::Int(now_ts())),
            ("rules".into(), Value::Int(self.rules as i64)),
            ("active".into(), Value::Int(self.active as i64)),
            ("folders".into(), strs(&self.folders)),
            ("unavailable".into(), strs(&self.unavailable)),
            // 0 = pauză până la reluare (ca în fișierul `pause`).
            ("paused_until".into(), opt_int(self.paused_until.map(|t| if t == i64::MAX { 0 } else { t }))),
            ("day".into(), Value::Str(self.day.clone())),
            ("done".into(), Value::Int(self.done as i64)),
            ("failed".into(), Value::Int(self.failed as i64)),
            ("last_error".into(), self.last_error.clone().map_or(Value::Null, Value::Str)),
            ("pending".into(), Value::Int(self.pending as i64)),
            (
                "scan".into(),
                self.scan.map_or(Value::Null, |(done, total, started)| {
                    Value::Obj(vec![
                        ("done".into(), Value::Int(done as i64)),
                        ("total".into(), Value::Int(total as i64)),
                        ("started".into(), Value::Int(started)),
                    ])
                }),
            ),
        ]))
    }
}

fn today() -> String {
    fmt_time(now_ts(), local_offset(), "AAAA-LL-ZZ", Lang::Ro)
}

/// Scrie starea atomic (temporar + redenumire).
fn write_status_file(path: &Path, st: &DaemonStatus) -> bool {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, st.to_json()).and_then(|()| std::fs::rename(&tmp, path)).is_ok()
}

/// Progresul scanării apare doar dacă durează mai mult de atât, apoi se actualizează la acest interval.
const SCAN_PROGRESS_AFTER: Duration = Duration::from_millis(400);
const SCAN_PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// Pauza din fișierul `pause`: `None` = activ, `Some(i64::MAX)` = până la reluare.
/// O pauză expirată se șterge.
fn read_pause(path: &Path) -> Option<i64> {
    let v: i64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    if v == 0 {
        Some(i64::MAX)
    } else if v > now_ts() {
        Some(v)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

/// Fișierele programului din folderul de date (nu se procesează, nu trezesc nimic).
struct ProgFiles {
    dir: PathBuf,
    rules: PathBuf,
    pause: PathBuf,
    settings: PathBuf,
}

impl ProgFiles {
    fn is_own(&self, p: &Path) -> bool {
        if !p.parent().is_some_and(|d| same_file(d, &self.dir)) {
            return false;
        }
        let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        n.starts_with(STATUS_FILE) || n.starts_with(SETTINGS_FILE) || n.starts_with("request-")
            || n.starts_with("organizator.db") || n.starts_with("daemon.")
    }
}

/// Evenimentele strânse între două treceri prin buclă.
#[derive(Default)]
struct Inbox {
    dirty: HashMap<PathBuf, Instant>,
    reload: Option<Instant>,
    requests: Vec<PathBuf>,
    pause_changed: bool,
    settings_changed: bool,
    /// Foldere apărute dintr-odată (mutate sau redenumite): fișierele din ele nu dau evenimente.
    dirs: Vec<PathBuf>,
    /// Watcher-ul a pierdut evenimente (buffer plin): trebuie o scanare completă.
    rescan: bool,
    /// The journal or update.json changed (a restore from the window, a check for updates):
    /// what the tray shows is looked at again.
    journal: bool,
}

struct State {
    engine: Engine,
    fs: SysFs,
    journal: Store,
    dicts: HashMap<String, Dictionary>,
    names: HashMap<u64, String>,
    roots: Vec<(PathBuf, bool)>,
    lang: Lang,
    logged: u64,
    status: DaemonStatus,
    status_written: Option<DaemonStatus>,
    status_path: PathBuf,
    /// update.json (empty with `--once`).
    update_path: PathBuf,
}

impl State {
    /// Scrie `daemon-status.json` doar dacă s-a schimbat ceva (atomic: temporar + redenumire).
    fn write_status(&mut self) {
        self.status.pending = self.journal.pending().len();
        let hidden: HashSet<u64> = self.engine.rules().iter().filter(|r| r.hidden).map(|r| r.id).collect();
        self.status.recent = recent_places(&self.journal.recent(RECENT_SCAN), &hidden, RECENT_SHOWN);
        if !self.update_path.as_os_str().is_empty() {
            self.status.update = org_update::read_state(&self.update_path).available().map(|v| v.to_string());
        }
        if self.status_path.as_os_str().is_empty() || self.status_written.as_ref() == Some(&self.status) {
            return;
        }
        if write_status_file(&self.status_path, &self.status) {
            self.status_written = Some(self.status.clone());
        }
        #[cfg(windows)]
        tray::update(&self.status);
    }

    fn set_rules_info(&mut self, unavailable: &[(PathBuf, bool)]) {
        self.status.rules = self.engine.rules().len();
        self.status.active = self.engine.rules().iter().filter(|r| r.enabled).count();
        self.status.folders = self.roots.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
        self.status.unavailable = unavailable.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
    }

    /// Cererile ferestrei („Marchează în așteptare”). Întoarce `true` dacă trebuie rescanat.
    fn apply_requests(&mut self, files: Vec<PathBuf>) -> bool {
        let mut any = false;
        for f in files {
            let text = match std::fs::read_to_string(&f) {
                Ok(t) => t,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    self.status.last_error = Some(format!("cerere {}: {e}", f.display()));
                    continue;
                }
            };
            match parse_request(&text) {
                Ok(Request::Pending { rule, paths }) => {
                    for p in &paths {
                        self.engine.mark_pending(&mut self.journal, rule, Some(p));
                    }
                    println!("[cerere] {} fișiere în așteptare pentru „{}”", paths.len(), self.rule_name(rule));
                    any = true;
                }
                Ok(Request::PendingRule { rule }) => {
                    self.engine.mark_pending(&mut self.journal, rule, None);
                    println!("[cerere] toate fișierele regulii „{}” în așteptare", self.rule_name(rule));
                    any = true;
                }
                Ok(Request::Approve { ids }) => self.answer(&ids, true),
                Ok(Request::Reject { ids }) => self.answer(&ids, false),
                Err(e) => {
                    eprintln!("[cerere] {}: {e}", f.display());
                    self.status.last_error = Some(format!("cerere invalidă: {e}"));
                }
            }
            let _ = std::fs::remove_file(&f);
        }
        if any {
            self.after();
        }
        any
    }

    /// Răspunsul din fereastră la confirmări: aprobat (pașii rămași rulează) sau respins.
    fn answer(&mut self, ids: &[u64], approve: bool) {
        let rc = RenderCtx::new(now_ts(), Tz::At(offset_at), self.lang, &self.dicts);
        for p in self.journal.pending().iter().filter(|p| ids.contains(&p.id)) {
            println!(
                "[confirmare] {} „{}”: {}",
                if approve { "aprobat" } else { "respins" },
                self.names.get(&p.rule_id).map(String::as_str).unwrap_or("?"),
                p.path.display()
            );
            if approve {
                self.engine.approve(&mut self.fs, &mut self.journal, &rc, p);
            } else {
                self.engine.reject(&mut self.fs, &mut self.journal, &rc, p);
            }
        }
        self.after();
    }

    fn rule_name(&self, id: u64) -> &str {
        self.names.get(&id).map(String::as_str).unwrap_or("?")
    }

    /// Scanare completă; întoarce fișierele care așteaptă și peste câte secunde să fie reluate.
    fn scan(&mut self) -> Vec<(PathBuf, i64)> {
        let rc = RenderCtx::new(now_ts(), Tz::At(offset_at), self.lang, &self.dicts);
        // Progresul, pentru fereastră: doar la scanările care durează (cel mult de ~2 ori pe secundă).
        let (path, mut shown) = (self.status_path.clone(), self.status.clone());
        let started = now_ts();
        let mut last: Option<Instant> = None;
        let mut wrote = false;
        let begun = Instant::now();
        let rep = self.engine.scan_with(&mut self.fs, &mut self.journal, &rc, &mut |done, total| {
            if path.as_os_str().is_empty() || done >= total || begun.elapsed() < SCAN_PROGRESS_AFTER {
                return;
            }
            if last.is_some_and(|t| t.elapsed() < SCAN_PROGRESS_EVERY) {
                return;
            }
            last = Some(Instant::now());
            shown.scan = Some((done, total, started));
            wrote |= write_status_file(&path, &shown);
        });
        self.status.scan = None;
        if wrote {
            // Fișierul arată încă progresul: trebuie rescris chiar dacă restul nu s-a schimbat.
            self.status_written = None;
        }
        self.after();
        rep.waiting_paths
    }

    /// Peste cât timp trebuie reluat fișierul (încă se modifică sau e deschis), dacă trebuie.
    fn handle(&mut self, path: &Path) -> Option<Duration> {
        let root = root_for(&self.roots, path).map(Path::to_path_buf)?;
        if !path.is_file() {
            return None;
        }
        let rc = RenderCtx::new(now_ts(), Tz::At(offset_at), self.lang, &self.dicts);
        let out = self.engine.process_path(&mut self.fs, &mut self.journal, &rc, path, &root);
        self.after();
        match out {
            // Se reia exact când ar trebui să fie gata (un interval lung nu înseamnă sondări dese).
            Outcome::Waiting => Some(Duration::from_secs(self.engine.settle_left(path, now_ts()) as u64)),
            Outcome::Busy => Some(IN_USE),
            _ => None,
        }
    }

    /// După fiecare procesare: afișează operațiile noi și salvează starea fișierelor.
    fn after(&mut self) {
        self.flush_log();
        let changes = self.engine.state.take_changes();
        if let Err(e) = self.journal.save_state_changes(&changes) {
            eprintln!("[eroare] nu s-a putut salva starea fișierelor: {e}");
            self.status.last_error = Some(format!("starea fișierelor: {e}"));
        }
        // Un catalog nesalvat doar se recalculează: nu e o eroare de arătat.
        if let Err(e) = self.journal.save_prints(&self.engine.prints().take_changes()) {
            eprintln!("[catalog] amprentele nu s-au putut salva: {e}");
        }
        if let Err(e) = self.journal.save_verdicts(&self.engine.prints().take_verdict_changes()) {
            eprintln!("[catalog] rezultatele condițiilor nu s-au putut salva: {e}");
        }
        self.write_status();
    }

    /// Scrie în consolă operațiile noi din jurnal.
    fn flush_log(&mut self) {
        let day = today();
        if self.status.day != day {
            self.status.day = day;
            self.status.done = 0;
            self.status.failed = 0;
        }
        for e in self.journal.entries_after(self.logged) {
            match e.status {
                org_journal::Status::Done => self.status.done += 1,
                org_journal::Status::Failed(_) => self.status.failed += 1,
                _ => {}
            }
            let rule = self.names.get(&e.rule_id).map(String::as_str).unwrap_or("?");
            let dst = e.dst.as_ref().map(|d| format!(" -> {}", d.display())).unwrap_or_default();
            println!("[{}] {rule}: {:?} {}{dst} [{:?}]", clock(self.lang), e.kind, e.src.display(), e.status);
            self.logged = e.id;
        }
    }
}

fn watch_all(
    watcher: &mut RecommendedWatcher,
    pending: Vec<(PathBuf, bool)>,
) -> Vec<(PathBuf, bool)> {
    let mut failed = Vec::new();
    for (p, rec) in pending {
        let mode = if rec { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        match watcher.watch(&p, mode) {
            Ok(()) => println!("[monitorizez] {}{}", p.display(), if rec { " (cu subfoldere)" } else { "" }),
            Err(e) => {
                println!("[indisponibil] {}: {e} — reîncerc la 30 s", p.display());
                failed.push((p, rec));
            }
        }
    }
    failed
}

struct Loaded {
    rules: Vec<Rule>,
    dicts: HashMap<String, Dictionary>,
    names: HashMap<u64, String>,
    roots: Vec<(PathBuf, bool)>,
}

/// Citește și validează fișierul de reguli. Id-urile se derivă din nume (stabile).
fn load_rules(path: &Path) -> Result<Loaded, Box<dyn Error>> {
    let text = std::fs::read_to_string(path)?;
    let bundle = org_portable::import(&text)?;
    let mut rules = bundle.rules;
    org_portable::assign_ids(&mut rules);
    let names = rules.iter().map(|r| (r.id, r.name.clone())).collect();
    // Folderele monitorizate (unice; recursiv are prioritate).
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    for r in rules.iter().filter(|r| r.enabled) {
        for f in &r.folders {
            match roots.iter_mut().find(|(p, _)| *p == f.path) {
                Some(existing) => existing.1 |= f.recursive,
                None => roots.push((f.path.clone(), f.recursive)),
            }
        }
    }
    Ok(Loaded { rules, dicts: bundle.dictionaries.into_iter().collect(), names, roots })
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    norm(a) == norm(b)
}

/// Un eveniment de la watcher: fișierul de reguli cere reîncărcare, restul sunt fișiere de procesat.
fn note(m: notify::Result<Event>, inbox: &mut Inbox, pf: &ProgFiles) {
    match m {
        Ok(ev) => {
            if ev.need_rescan() {
                inbox.rescan = true;
            }
            // Un folder mutat sau redenumit apare cu un singur eveniment (al lui).
            if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))) {
                for p in &ev.paths {
                    if !pf.is_own(p) && p.is_dir() && !inbox.dirs.contains(p) {
                        inbox.dirs.push(p.clone());
                    }
                }
            }
            // „Reia” șterge fișierul de pauză: singura ștergere care contează.
            if matches!(ev.kind, EventKind::Remove(_)) && ev.paths.iter().any(|p| same_file(p, &pf.pause)) {
                inbox.pause_changed = true;
            }
            if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                for p in ev.paths {
                    if same_file(&p, &pf.rules) {
                        inbox.reload = Some(Instant::now() + Duration::from_millis(300));
                    } else if same_file(&p, &pf.settings) {
                        inbox.settings_changed = true;
                    } else if same_file(&p, &pf.pause) {
                        inbox.pause_changed = true;
                    } else if is_request_file(&p) && p.parent().is_some_and(|d| same_file(d, &pf.dir)) {
                        if !inbox.requests.contains(&p) {
                            inbox.requests.push(p);
                        }
                    } else if !pf.is_own(&p) {
                        inbox.dirty.insert(p, Instant::now());
                    } else if p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy().to_lowercase();
                        n.starts_with("organizator.db") || n.starts_with(org_update::STATE_FILE)
                    }) {
                        inbox.journal = true;
                    }
                }
            }
        }
        Err(e) => {
            // Erorile watcher-ului (de obicei buffer plin) pot însemna evenimente pierdute.
            eprintln!("[watcher] {e} — rescanez");
            inbox.rescan = true;
        }
    }
}

/// Câte fișiere se iau dintr-un folder apărut dintr-odată; peste, se face scanare completă.
const DIR_WALK_MAX: usize = 50_000;

/// Fișierele dintr-un folder apărut dintr-odată (cu subfoldere), sau `None` dacă sunt prea multe.
fn walk_files(dir: &Path) -> Option<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            match e.file_type() {
                // Legăturile simbolice spre foldere nu se urmează (bucle).
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => {
                    out.push(e.path());
                    if out.len() > DIR_WALK_MAX {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
    Some(out)
}

fn main() {
    #[cfg(windows)]
    console::attach_parent();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = matches!(args.first().map(String::as_str), Some("journal") | Some("jurnal") | Some("undo"));
    let use_tray = cfg!(windows) && !cli && !args.iter().any(|a| a == "--once" || a == "--no-tray");

    #[cfg(windows)]
    {
        if use_tray {
            let lang = match args.iter().position(|a| a == "--lang").and_then(|i| args.get(i + 1)).map(String::as_str) {
                Some("en") => Lang::En,
                Some("ro") => Lang::Ro,
                _ => saved_lang(),
            };
            tray::run_in_background(lang, run);
            return;
        }
    }
    let _ = use_tray;

    if let Err(e) = run(|| {}) {
        eprintln!("eroare: {e}");
        std::process::exit(1);
    }
}

/// Procesul are subsistemul „windows” (fără consolă la pornirea automată). Când e lansat dintr-un
/// terminal, se atașează la el ca `println!` să se vadă.
#[cfg(windows)]
mod console {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    pub fn attach_parent() {
        // SAFETY: apeluri Win32 simple; handle-ul „CONOUT$” rămâne deschis cât trăiește procesul.
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            if !(out.is_null() || out == INVALID_HANDLE_VALUE) {
                return; // ieșirea e deja redirecționată (fișier, pipe) sau există consolă
            }
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return; // pornit fără terminal (autostart, interfață): rămâne silențios
            }
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                let h = f.as_raw_handle();
                SetStdHandle(STD_OUTPUT_HANDLE, h as _);
                SetStdHandle(STD_ERROR_HANDLE, h as _);
                std::mem::forget(f);
            }
        }
    }
}

/// Pictograma din zona de notificări. Daemonul rulează pe un fir separat; firul principal
/// ține bucla de mesaje Win32 de care are nevoie pictograma.
#[cfg(windows)]
mod tray {
    use super::{DaemonStatus, Lang};
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{mpsc, Mutex, OnceLock};
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_APP,
    };

    /// Firul pictogramei (pentru mesajul de actualizare) și ce trebuie arătat:
    /// (pauză până la, operații azi, erori azi).
    static TRAY_TID: AtomicU32 = AtomicU32::new(0);
    /// (pauză până la, operații azi, erori azi, confirmări în așteptare).
    type Info = (Option<i64>, u64, u64, usize);
    static INFO: Mutex<Option<Info>> = Mutex::new(None);
    static PAUSE_PATH: OnceLock<PathBuf> = OnceLock::new();
    /// Where the latest files went (from the daemon), and the menu items that open them.
    static RECENT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    /// A newer version on GitHub, shown at the top of the menu.
    static UPDATE: Mutex<Option<String>> = Mutex::new(None);
    static RECENT_IDS: Mutex<Vec<(MenuId, PathBuf)>> = Mutex::new(Vec::new());

    pub fn set_pause_path(p: PathBuf) {
        let _ = PAUSE_PATH.set(p);
    }

    /// Apelat de firul daemonului: pictograma își actualizează textul și meniul.
    pub fn update(st: &DaemonStatus) {
        if let Ok(mut g) = INFO.lock() {
            *g = Some((st.paused_until, st.done, st.failed, st.pending));
        }
        if let Ok(mut g) = RECENT.lock() {
            g.clone_from(&st.recent);
        }
        if let Ok(mut g) = UPDATE.lock() {
            g.clone_from(&st.update);
        }
        let tid = TRAY_TID.load(Ordering::Acquire);
        if tid != 0 {
            // SAFETY: mesaj fără date către firul pictogramei.
            unsafe {
                PostThreadMessageW(tid, WM_APP, 0, 0);
            }
        }
    }

    /// Pauza se pune prin același fișier ca din fereastră; daemonul îl vede și se oprește.
    fn write_pause(until: Option<i64>) {
        let Some(p) = PAUSE_PATH.get() else { return };
        let _ = match until {
            Some(t) => std::fs::write(p, t.to_string()),
            None => std::fs::remove_file(p),
        };
    }

    /// The "Recent files" submenu: one item per place, rebuilt when the list changes.
    struct Recent {
        menu: Submenu,
        empty: MenuItem,
        items: Vec<MenuItem>,
        shown: Option<Vec<PathBuf>>,
    }

    impl Recent {
        fn refresh(&mut self) {
            let now = RECENT.lock().map(|g| g.clone()).unwrap_or_default();
            if self.shown.as_ref() == Some(&now) {
                return;
            }
            for it in self.items.drain(..) {
                let _ = self.menu.remove(&it);
            }
            let _ = self.menu.remove(&self.empty);
            let mut ids = Vec::with_capacity(now.len());
            if now.is_empty() {
                let _ = self.menu.append(&self.empty);
            }
            for p in &now {
                let it = MenuItem::new(label(p), true, None);
                let _ = self.menu.append(&it);
                ids.push((it.id().clone(), p.clone()));
                self.items.push(it);
            }
            if let Ok(mut g) = RECENT_IDS.lock() {
                *g = ids;
            }
            self.shown = Some(now);
        }
    }

    /// "invoice.pdf<tab>…\Invoices\2026-11": the name, and the folder in the right column.
    fn label(p: &Path) -> String {
        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
        // A single & would underline the next letter in a Windows menu.
        format!("{}\t{}", shorten(&name, 48, false), shorten(&dir, 42, true)).replace('&', "&&")
    }

    fn shorten(s: &str, max: usize, keep_end: bool) -> String {
        let n = s.chars().count();
        if n <= max {
            s.to_string()
        } else if keep_end {
            format!("…{}", s.chars().skip(n + 1 - max).collect::<String>())
        } else {
            format!("{}…", s.chars().take(max - 1).collect::<String>())
        }
    }

    /// Shows a file in Explorer, selected; a folder opens; a file that is no longer there
    /// opens the nearest folder that still is.
    fn reveal(p: PathBuf) {
        std::thread::spawn(move || {
            use std::os::windows::process::CommandExt;
            let mut cmd = std::process::Command::new("explorer.exe");
            if p.is_dir() {
                cmd.arg(&p);
            } else if p.exists() {
                // Explorer wants `/select,"path"` as one argument, with its own quotes.
                cmd.raw_arg(format!("/select,\"{}\"", p.display()));
            } else if let Some(d) = p.ancestors().skip(1).find(|d| d.is_dir()) {
                cmd.arg(d);
            } else {
                return;
            }
            let _ = cmd.spawn();
        });
    }

    /// "FilePilot 1.4.0 is available" at the top of the menu, while there is one.
    struct UpdateItem {
        menu: Menu,
        item: MenuItem,
        shown: Option<String>,
    }

    impl UpdateItem {
        fn refresh(&mut self, lang: Lang) {
            let now = UPDATE.lock().ok().and_then(|g| g.clone());
            if now == self.shown {
                return;
            }
            if self.shown.is_some() {
                let _ = self.menu.remove(&self.item);
            }
            if let Some(v) = &now {
                self.item.set_text(match lang {
                    Lang::En => format!("FilePilot {v} is available…"),
                    _ => format!("FilePilot {v} e disponibil…"),
                });
                let _ = self.menu.insert(&self.item, 0);
            }
            self.shown = now;
        }
    }

    fn apply(tray: &TrayIcon, pause1: &MenuItem, pause_inf: &MenuItem, resume: &MenuItem, recent: &mut Recent, upd: &mut UpdateItem, lang: Lang) {
        recent.refresh();
        upd.refresh(lang);
        let (paused, done, failed, pending) = INFO.lock().ok().and_then(|g| *g).unwrap_or((None, 0, 0, 0));
        let now = super::now_ts();
        let is_paused = paused.is_some_and(|u| u > now);
        let en = matches!(lang, Lang::En);
        let tip = if is_paused {
            match paused {
                Some(i64::MAX) | None => if en { "FilePilot: paused until resumed".to_string() } else { "FilePilot: în pauză până la reluare".to_string() },
                Some(u) => {
                    let at = super::fmt_time(u, super::offset_at(u), "HH:MM", lang);
                    if en { format!("FilePilot: paused until {at}") } else { format!("FilePilot: în pauză până la {at}") }
                }
            }
        } else if en {
            format!("FilePilot: today {done} operations, {failed} errors")
        } else {
            format!("FilePilot: azi {done} operații, {failed} erori")
        };
        let tip = match (pending, en) {
            (0, _) => tip,
            (n, true) => format!("{tip}\n{n} waiting for confirmation"),
            (n, false) => format!("{tip}\n{n} de confirmat"),
        };
        let _ = tray.set_tooltip(Some(tip));
        pause1.set_enabled(!is_paused);
        pause_inf.set_enabled(!is_paused);
        resume.set_enabled(is_paused);
    }

    /// 32x32 RGBA brut (generat din icons/32x32.png).
    const ICON_RGBA: &[u8] = include_bytes!("tray.rgba");

    fn log_error(msg: &str) {
        if let Ok(f) = org_paths::DataFiles::default_ensured() {
            let _ = std::fs::write(f.dir.join("daemon.log"), msg);
        }
    }

    /// Pornește fereastra prin Explorer, ca la dublu-click (mediu și mânere curate, fără moștenire
    /// de la daemon), pe un fir separat ca să nu blocheze bucla de mesaje a pictogramei.
    fn open_ui() {
        std::thread::spawn(|| {
            if let Ok(exe) = std::env::current_exe() {
                let ui = exe.with_file_name("filepilot-ui.exe");
                if std::process::Command::new("explorer.exe").arg(&ui).spawn().is_err() {
                    let _ = std::process::Command::new(&ui).spawn();
                }
            }
        });
    }

    pub fn run_in_background(
        lang: Lang,
        daemon: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), Box<dyn Error>> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let ready: Box<dyn FnOnce() + Send> = Box::new(move || {
                let _ = tx.send(());
            });
            if let Err(e) = daemon(ready) {
                log_error(&format!("eroare: {e}\n"));
                std::process::exit(1);
            }
            std::process::exit(0);
        });
        // Fără semnal „gata” (altă instanță rulează deja sau a apărut o eroare) nu apare pictogramă.
        if rx.recv().is_err() {
            std::process::exit(1);
        }

        let (open_t, quit_t, tip, p1_t, pinf_t, res_t) = match lang {
            Lang::En => ("Open FilePilot", "Quit", "FilePilot is running in the background", "Pause for 1 hour", "Pause until resumed", "Resume"),
            _ => ("Deschide FilePilot", "Ieșire", "FilePilot rulează în fundal", "Pauză o oră", "Pauză până reiau", "Reia"),
        };
        let (recent_t, none_t) = match lang {
            Lang::En => ("Recent files", "No files yet"),
            _ => ("Fișiere recente", "Niciun fișier încă"),
        };
        let mut recent = Recent { menu: Submenu::new(recent_t, true), empty: MenuItem::new(none_t, false, None), items: Vec::new(), shown: None };
        let menu = Menu::new();
        let open = MenuItem::new(open_t, true, None);
        let pause1 = MenuItem::new(p1_t, true, None);
        let pause_inf = MenuItem::new(pinf_t, true, None);
        let resume = MenuItem::new(res_t, false, None);
        let quit = MenuItem::new(quit_t, true, None);
        let _ = menu.append(&open);
        let _ = menu.append(&recent.menu);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&pause1);
        let _ = menu.append(&pause_inf);
        let _ = menu.append(&resume);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit);
        // The menu is shared (Rc): the copy kept here adds and removes the update item.
        let mut upd = UpdateItem { menu: menu.clone(), item: MenuItem::new("", true, None), shown: None };
        let icon = Icon::from_rgba(ICON_RGBA.to_vec(), 32, 32).expect("pictograma tray");
        let tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip(tip)
            .with_icon(icon)
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                log_error(&format!("pictograma tray: {e}\n"));
                // Fără pictogramă daemonul tot lucrează; fereastra îl poate opri doar din Task Manager.
                loop {
                    std::thread::park();
                }
            }
        };
        let (open_id, quit_id, upd_id) = (open.id().clone(), quit.id().clone(), upd.item.id().clone());
        let (p1_id, pinf_id, res_id) = (pause1.id().clone(), pause_inf.id().clone(), resume.id().clone());
        // Handlere apelate direct din procedura ferestrei pictogramei: reacție imediată, fără să
        // depindă de următorul mesaj Windows.
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == open_id || e.id == upd_id {
                open_ui();
            } else if e.id == p1_id {
                write_pause(Some(super::now_ts() + 3600));
            } else if e.id == pinf_id {
                write_pause(Some(0));
            } else if e.id == res_id {
                write_pause(None);
            } else if e.id == quit_id {
                // Jurnalul e write-ahead și SQLite e sincron: oprirea promptă e sigură,
                // iar la pornirea următoare `recover()` reconciliază ce a rămas neterminat.
                std::process::exit(0);
            } else {
                let place = RECENT_IDS.lock().ok().and_then(|g| g.iter().find(|(id, _)| *id == e.id).map(|(_, p)| p.clone()));
                if let Some(p) = place {
                    reveal(p);
                }
            }
        }));
        TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                open_ui();
            }
        }));

        // SAFETY: buclă standard de mesaje pe firul care a creat pictograma. WM_APP fără
        // fereastră e mesajul de actualizare trimis de `update`.
        unsafe {
            TRAY_TID.store(GetCurrentThreadId(), Ordering::Release);
            apply(&tray, &pause1, &pause_inf, &resume, &mut recent, &mut upd, lang);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() && msg.message == WM_APP {
                    apply(&tray, &pause1, &pause_inf, &resume, &mut recent, &mut upd, lang);
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn cli_journal(args: &[String]) -> Result<(), Box<dyn Error>> {
    let db = args.first().ok_or("usage: filepilot journal <file.db> [--limit N]")?;
    let mut limit = 20u32;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        if a == "--limit" {
            limit = it.next().and_then(|v| v.parse().ok()).ok_or("--limit cere un număr")?;
        } else {
            return Err(format!("opțiune necunoscută: {a}").into());
        }
    }
    let store = Store::open(Path::new(db))?;
    let mut entries = store.recent(limit);
    entries.reverse();
    for e in entries {
        let dst = e.dst.as_ref().map(|d| format!(" -> {}", d.display())).unwrap_or_default();
        println!(
            "#{} run {} [{}] {:?} {}{dst} [{:?}]",
            e.id,
            e.run_id,
            fmt_time(e.ts, local_offset(), "AAAA-LL-ZZ HH:MM:SS", Lang::Ro),
            e.kind,
            e.src.display(),
            e.status
        );
    }
    Ok(())
}

fn print_outcome(id: u64, r: io::Result<UndoOutcome>) {
    match r {
        Ok(UndoOutcome::Restored(p)) => println!("#{id}: restaurat în {}", p.display()),
        Ok(UndoOutcome::NeedsConfirmation(why)) => {
            println!("#{id}: nu s-a restaurat: {why}. Repetă cu --force dacă ești sigur.")
        }
        Ok(UndoOutcome::Unrecoverable(why)) => println!("#{id}: nu se poate restaura: {why}"),
        Ok(UndoOutcome::NotUndoable) => println!("#{id}: operația nu se poate anula"),
        Err(e) => println!("#{id}: eroare: {e}"),
    }
}

fn cli_undo(args: &[String]) -> Result<(), Box<dyn Error>> {
    let usage = "usage: filepilot undo <file.db> <id> | --run <run> [--force]";
    let db = args.first().ok_or(usage)?;
    let (mut force, mut run, mut id) = (false, false, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => force = true,
            "--run" => {
                run = true;
                id = Some(it.next().and_then(|v| v.parse::<u64>().ok()).ok_or(usage)?);
            }
            x => id = Some(x.parse::<u64>().map_err(|_| usage)?),
        }
    }
    let id = id.ok_or(usage)?;
    let mut store = Store::open(Path::new(db))?;
    let mut fs = SysFs;
    if run {
        let results = undo_run(&mut fs, &mut store, id, force);
        if results.is_empty() {
            println!("rularea {id}: nimic de anulat");
        }
        for (eid, r) in results {
            print_outcome(eid, r);
        }
    } else {
        print_outcome(id, undo(&mut fs, &mut store, id, force));
    }
    Ok(())
}

fn run(ready: impl FnOnce()) -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("journal") | Some("jurnal") => return cli_journal(&args[1..]),
        Some("undo") => return cli_undo(&args[1..]),
        _ => {}
    }
    let opts = parse_args()?;

    // Fișierele: cele din folderul de date (implicit) sau cele date explicit.
    let (rules_path, db_path, lock_path) = match &opts.rules {
        Some(r) => (r.clone(), opts.db.clone().unwrap_or_else(|| r.with_extension("db")), r.with_extension("lock")),
        None => {
            let f = DataFiles::default_ensured()?;
            if !f.rules.exists() {
                std::fs::write(&f.rules, org_portable::export(&[], &Default::default(), true))?;
            }
            (f.rules, opts.db.clone().unwrap_or(f.db), f.lock)
        }
    };

    // O singură instanță de daemon (nu și pentru `--once`).
    let _lock = if opts.once {
        None
    } else {
        match acquire_lock(&lock_path)? {
            Some(l) => Some(l),
            None => return Err("un alt daemon rulează deja".into()),
        }
    };

    // Instanța e unică: de aici poate apărea pictograma din tray.
    ready();

    let loaded = load_rules(&rules_path)?;
    println!("{} reguli încărcate din {}", loaded.rules.len(), rules_path.display());
    if loaded.roots.is_empty() {
        println!("nicio regulă activă nu monitorizează un folder; aștept reguli noi");
    }

    let mut journal = Store::open(&db_path)?;
    println!("baza de date: {}", db_path.display());
    let mut fs = SysFs;

    // Operațiile rămase neterminate după o oprire bruscă se reconciliază înainte de orice.
    for (id, status) in recover(&mut fs, &mut journal) {
        println!("[recuperare] intrarea {id}: {status:?}");
    }
    // Curățenie: jurnalul mai vechi decât s-a ales în Setări și fișiere care nu mai există.
    prune(&mut journal);
    journal.forget_missing(|p| p.exists())?;

    let rules_dir = rules_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let pf = ProgFiles {
        dir: rules_dir.clone(),
        rules: rules_path.clone(),
        pause: rules_dir.join(PAUSE_FILE),
        settings: rules_dir.join(SETTINGS_FILE),
    };
    #[cfg(windows)]
    tray::set_pause_path(pf.pause.clone());
    let mut engine = Engine::new(loaded.rules, opts.settle.unwrap_or_else(saved_settle));
    // Fișierele programului nu se ating, chiar dacă stau într-un folder monitorizat
    // (prefixe: acoperă și `-wal`, `-shm`, `.tmp`).
    let own = [
        rules_dir.join(STATUS_FILE),
        rules_dir.join(SETTINGS_FILE),
        rules_dir.join("request-"),
        pf.pause.clone(),
        rules_dir.join(BACKUP_DIR),
    ];
    engine.exclude_prefixes(&[&rules_path, &db_path, &lock_path, &own[0], &own[1], &own[2], &own[3], &own[4]]);
    for (rule, path, stat) in journal.load_file_state()? {
        engine.state.load(rule, path, stat);
    }
    engine.load_pending(&journal.pending());
    // Catalogul amprentelor: fișierele mari deja văzute nu se recitesc la căutarea dublurilor.
    // Plus rezultatele condițiilor pe conținut: textul PDF-urilor neschimbate nu se reextrage.
    match (journal.load_prints(), journal.load_verdicts()) {
        (Ok(rows), Ok(verdicts)) => engine.set_prints(org_engine::Prints::from_rows(rows).with_verdicts(verdicts)),
        (Err(e), _) | (_, Err(e)) => eprintln!("[catalog] nu s-a putut încărca: {e}"),
    }
    // Regulile s-au putut schimba cât daemonul a fost oprit.
    let dropped = engine.drop_stale_pending(&mut journal);
    if dropped > 0 {
        println!("[confirmare] {dropped} cereri renunțate: regula nu mai cere confirmare");
    }
    let logged = journal.last_id();

    let mut st = State {
        engine,
        fs,
        journal,
        dicts: loaded.dicts,
        names: loaded.names,
        roots: loaded.roots.clone(),
        lang: opts.lang,
        logged,
        status: DaemonStatus { started: now_ts(), day: today(), paused_until: read_pause(&pf.pause), ..Default::default() },
        status_written: None,
        // `--once` (test din terminal) nu lasă fișiere de stare lângă reguli.
        status_path: if opts.once { PathBuf::new() } else { rules_dir.join(STATUS_FILE) },
        update_path: if opts.once { PathBuf::new() } else { rules_dir.join(org_update::STATE_FILE) },
    };
    st.set_rules_info(&[]);
    // Cererile scrise cât daemonul era oprit se aplică înainte de prima scanare.
    st.apply_requests(pending_requests(&rules_dir));
    st.write_status();

    // Scanarea inițială (nu și în pauză); fișierele încă în modificare se reiau.
    let paused_at_start = st.status.paused_until.is_some() && !opts.once;
    if paused_at_start {
        println!("[pauză] nu procesez până la reluare");
    }
    let waiting = if paused_at_start { Vec::new() } else { st.scan() };
    if opts.once {
        let (mut waiting, mut tries) = (waiting, 0);
        while !waiting.is_empty() && tries < 120 {
            std::thread::sleep(RESCAN);
            waiting = st.scan();
            tries += 1;
        }
        println!("gata: {} operații în jurnal", st.journal.count());
        return Ok(());
    }
    let mut rescan_due: Option<Instant> = None;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
    let mut unavailable = watch_all(&mut watcher, loaded.roots);
    let mut next_retry = Instant::now() + RETRY_ROOTS;
    st.set_rules_info(&unavailable);
    st.write_status();

    // Folderul de date: de aici aflăm când fereastra modifică regulile, cere ceva sau pune pauză.
    if let Err(e) = watcher.watch(&rules_dir, RecursiveMode::NonRecursive) {
        eprintln!("[reguli] nu pot urmări {}: {e}", rules_dir.display());
    }

    println!("rulez; Ctrl+C pentru oprire");
    let mut inbox = Inbox::default();
    let mut retry: HashMap<PathBuf, Instant> = HashMap::new();
    // Fișierele care așteaptă se reiau fiecare la timpul lui, fără rescanări complete.
    let schedule = |retry: &mut HashMap<PathBuf, Instant>, waiting: Vec<(PathBuf, i64)>| {
        for (p, secs) in waiting {
            retry.insert(p, Instant::now() + Duration::from_secs(secs.max(1) as u64));
        }
    };
    schedule(&mut retry, waiting);
    let mut next_periodic = Instant::now() + PERIODIC;
    let mut last_prune = Instant::now();
    let mut was_paused = paused_at_start;
    let mut next_update_look = Instant::now() + UPDATE_FIRST_LOOK;

    loop {
        let paused_until = st.status.paused_until;
        let paused = paused_until.is_some_and(|u| now_ts() < u);
        let busy = !inbox.dirty.is_empty() || !retry.is_empty() || rescan_due.is_some() || inbox.reload.is_some();
        // Fără treabă, firul doarme până la cea mai apropiată reîncercare (cel mult IDLE).
        // În pauză nu se lucrează: se doarme până la sfârșitul ei (sau un eveniment).
        let wait = match st.engine.next_retry_at() {
            _ if paused && inbox.reload.is_none() => Duration::from_secs(paused_until.map_or(60, |u| (u - now_ts()).clamp(1, IDLE.as_secs() as i64)) as u64),
            _ if busy => BUSY,
            Some(t) => Duration::from_secs((t - now_ts()).clamp(1, IDLE.as_secs() as i64) as u64),
            None => IDLE,
        };
        match rx.recv_timeout(wait) {
            Ok(m) => note(m, &mut inbox, &pf),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(m) = rx.try_recv() {
            note(m, &mut inbox, &pf);
        }

        // Pauza: pusă sau scoasă din tray sau din fereastră (fișierul `pause`), sau expirată.
        if std::mem::take(&mut inbox.pause_changed) || paused_until.is_some_and(|u| now_ts() >= u) {
            st.status.paused_until = read_pause(&pf.pause);
            st.write_status();
        }
        let paused = st.status.paused_until.is_some_and(|u| now_ts() < u);
        if paused != was_paused {
            println!("[pauză] {}", if paused { "pornită" } else { "încheiată; recuperez ce s-a schimbat" });
            if !paused {
                // Tot ce s-a schimbat între timp se prinde cu o scanare completă.
                rescan_due = Some(Instant::now());
            }
            was_paused = paused;
        }

        // The journal changed outside the daemon (a restore): the tray menu follows.
        if std::mem::take(&mut inbox.journal) {
            st.write_status();
        }

        // Cererile ferestrei („Marchează în așteptare”) se aplică și în pauză.
        if !inbox.requests.is_empty() {
            let reqs = std::mem::take(&mut inbox.requests);
            if st.apply_requests(reqs) && !paused {
                rescan_due = Some(Instant::now());
            }
            st.write_status();
        }

        // Interfața a modificat regulile: reîncarcă (dacă fișierul e invalid, rămân cele vechi).
        if inbox.reload.is_some_and(|t| Instant::now() >= t) {
            inbox.reload = None;
            match load_rules(&rules_path) {
                Ok(l) => {
                    for (p, _) in st.roots.iter().chain(unavailable.iter()) {
                        let _ = watcher.unwatch(p);
                    }
                    st.engine.set_rules(l.rules);
                    let dropped = st.engine.drop_stale_pending(&mut st.journal);
                    if dropped > 0 {
                        println!("[confirmare] {dropped} cereri renunțate: regula nu mai cere confirmare");
                    }
                    st.names = l.names;
                    st.dicts = l.dicts;
                    st.roots = l.roots.clone();
                    unavailable = watch_all(&mut watcher, l.roots);
                    let _ = watcher.watch(&rules_dir, RecursiveMode::NonRecursive);
                    rescan_due = Some(Instant::now());
                    println!("[reguli] reîncărcate: {} reguli", st.engine.rules().len());
                    st.set_rules_info(&unavailable);
                    st.status.last_error = None;
                }
                Err(e) => {
                    eprintln!("[reguli] fișier invalid, păstrez regulile vechi: {e}");
                    st.status.last_error = Some(format!("reguli invalide (rămân cele vechi): {e}"));
                }
            }
            st.write_status();
        }

        // Setările ferestrei: intervalul de așteptare se schimbă pe loc.
        if std::mem::take(&mut inbox.settings_changed) && opts.settle.is_none() {
            let s = saved_settle();
            if s != st.engine.settle_secs {
                println!("[setări] procesez după {s} s fără modificări");
                st.engine.settle_secs = s;
                // Fișierele care așteaptă se reevaluează cu noul interval.
                for t in retry.values_mut() {
                    *t = Instant::now();
                }
            }
        }

        // Evenimente pierdute: scanarea completă prinde tot (și în pauză, la reluare).
        if std::mem::take(&mut inbox.rescan) && !paused {
            rescan_due = Some(Instant::now());
        }

        if paused {
            inbox.dirs.clear(); // scanarea de la reluare le acoperă
            // Evenimentele se strâng; dacă sunt prea multe, scanarea de la reluare le acoperă.
            if inbox.dirty.len() > 5000 {
                inbox.dirty.clear();
            }
            retry.clear();
            continue;
        }

        // Foldere apărute dintr-odată: fișierele lor intră la procesare ca și cum ar fi apărut acum.
        // Doar sub un folder monitorizat cu subfoldere; altfel nu sunt în aria regulilor.
        for d in std::mem::take(&mut inbox.dirs) {
            let recursive = st.roots.iter().filter(|(r, _)| d.starts_with(r) && d != *r).any(|(_, rec)| *rec);
            if !recursive {
                continue;
            }
            match walk_files(&d) {
                Some(files) => {
                    if !files.is_empty() {
                        println!("[folder nou] {}: {} fișiere", d.display(), files.len());
                    }
                    let now = Instant::now();
                    for f in files {
                        inbox.dirty.entry(f).or_insert(now);
                    }
                }
                None => rescan_due = Some(Instant::now()),
            }
        }

        // Evenimentele liniștite de cel puțin DEBOUNCE se procesează.
        let due: Vec<PathBuf> =
            inbox.dirty.iter().filter(|(_, t)| t.elapsed() >= DEBOUNCE).map(|(p, _)| p.clone()).collect();
        for p in due {
            inbox.dirty.remove(&p);
            if let Some(wait) = st.handle(&p) {
                retry.insert(p, Instant::now() + wait);
            }
        }

        // Fișierele care așteptau să devină stabile.
        let again: Vec<PathBuf> =
            retry.iter().filter(|(_, t)| Instant::now() >= **t).map(|(p, _)| p.clone()).collect();
        for p in again {
            retry.remove(&p);
            if let Some(wait) = st.handle(&p) {
                retry.insert(p, Instant::now() + wait);
            }
        }

        // Reîncercările după eșecuri temporare (fișier blocat, unitate lipsă).
        for p in st.engine.take_due_retries(now_ts()) {
            if let Some(wait) = st.handle(&p) {
                retry.insert(p, Instant::now() + wait);
            }
        }

        // Updates, only with the user's consent: GitHub is asked once a day, in the background.
        if Instant::now() >= next_update_look && !opts.once {
            next_update_look = Instant::now() + UPDATE_LOOK;
            check_updates_if_due(&rules_dir);
        }

        // Condițiile relative la timp: rescanare periodică.
        if Instant::now() >= next_periodic {
            next_periodic = Instant::now() + PERIODIC;
            // O dată pe zi: jurnalul vechi (daemonul poate rula săptămâni fără repornire).
            if last_prune.elapsed() >= Duration::from_secs(86_400) {
                last_prune = Instant::now();
                prune(&mut st.journal);
                // Amprentele fișierelor care nu mai există.
                let gone = st.engine.prints().remove_missing(&st.fs);
                if !gone.is_empty() {
                    match st.journal.delete_prints(&gone) {
                        Ok(()) => println!("[catalog] scoase: {} fișiere dispărute", gone.len()),
                        Err(e) => eprintln!("[dubluri] curățenie: {e}"),
                    }
                }
            }
            if st.engine.needs_periodic_scan() && rescan_due.is_none() {
                rescan_due = Some(Instant::now());
            }
        }

        if let Some(t) = rescan_due {
            if Instant::now() >= t {
                rescan_due = None;
                let waiting = st.scan();
                schedule(&mut retry, waiting);
            }
        }

        // Foldere monitorizate: cele dispărute (unitate scoasă, rețea căzută, folder șters) trec
        // la indisponibile; cele indisponibile se reîncearcă. La revenire, scanare completă.
        if Instant::now() >= next_retry {
            let gone: Vec<(PathBuf, bool)> = st
                .roots
                .iter()
                .filter(|r| !unavailable.contains(r) && !r.0.is_dir())
                .cloned()
                .collect();
            let mut changed = false;
            for (p, rec) in gone {
                println!("[indisponibil] {} a dispărut — reîncerc la 30 s", p.display());
                let _ = watcher.unwatch(&p);
                unavailable.push((p, rec));
                changed = true;
            }
            if !unavailable.is_empty() {
                let before = unavailable.len();
                unavailable = watch_all(&mut watcher, std::mem::take(&mut unavailable));
                if unavailable.len() < before {
                    rescan_due = Some(Instant::now());
                    changed = true;
                }
            }
            if changed {
                st.set_rules_info(&unavailable);
                st.write_status();
            }
            next_retry = Instant::now() + RETRY_ROOTS;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: u64, rule: u64, kind: OpKind, src: &str, dst: Option<&str>, status: Status) -> Entry {
        Entry {
            id,
            run_id: id,
            rule_id: rule,
            ts: 0,
            kind,
            src: PathBuf::from(src),
            dst: dst.map(PathBuf::from),
            size: 0,
            mtime: 0,
            status,
        }
    }

    #[test]
    fn the_tray_lists_where_files_ended_up() {
        // Newest first, as the journal gives them.
        let entries = vec![
            e(9, 1, OpKind::Rename, "/inv/2026/a.pdf", Some("/inv/2026/Enel a.pdf"), Status::Done),
            e(8, 1, OpKind::Move, "/dl/a.pdf", Some("/inv/2026/a.pdf"), Status::Done),
            e(7, 2, OpKind::Move, "/dl/secret.pdf", Some("/vault/secret.pdf"), Status::Done),
            e(6, 1, OpKind::Move, "/dl/b.pdf", Some("/inv/b.pdf"), Status::Failed("busy".into())),
            e(5, 1, OpKind::Copy, "/dl/c.pdf", Some("/backup/c.pdf"), Status::Done),
            e(4, 1, OpKind::Trash, "/dl/d.tmp", None, Status::Done),
            e(3, 1, OpKind::Extract, "/dl/e.zip", Some("/dl/e"), Status::Done),
            e(2, 1, OpKind::Move, "/dl/f.pdf", Some("/inv/f.pdf"), Status::Undone),
            e(1, 1, OpKind::Move, "/old/c.pdf", Some("/backup/c.pdf"), Status::Done),
        ];
        let hidden: HashSet<u64> = [2].into_iter().collect();
        let got = recent_places(&entries, &hidden, 10);
        let want: Vec<PathBuf> = ["/inv/2026/Enel a.pdf", "/backup/c.pdf", "/dl/e"].iter().map(PathBuf::from).collect();
        assert_eq!(got, want);
        assert_eq!(recent_places(&entries, &hidden, 2).len(), 2);
    }
}
