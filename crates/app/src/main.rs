// În versiunea de producție nu se deschide fereastră de consolă.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Fereastra organizatorului. Nu rulează reguli: editează `rules.orgrules`, citește jurnalul și
//! cere restaurări. Daemonul (`filepilot.exe`) face munca și își reîncarcă singur regulile.
//!
//! Comenzile întorc text JSON (format `org_portable::json`), ca tipurile din nucleu să rămână
//! fără dependențe.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use org_actions::plan_rule;
use org_core::{timefmt::Lang, EvalCtx, FileMeta, Prepared, RenderCtx, Tz};
use org_fs::{DryFs, Fs};
use org_journal::{undo, undo_run, Entry, Journal, NewEntry, OpKind, Status, UndoOutcome};
use org_paths::{backup_rules, daemon_running, rule_backups, write_request, DataFiles, BACKUP_DIR, PAUSE_FILE, STATUS_FILE};
use org_portable::json::{self, Value};
use org_portable::request::{request_to_json, Request};
use org_core::secret::{new_salt, PasswordHash};
use org_core::ContentLoader;
use org_engine::{DirCache, FsContent, PrintRow, Prints, VerdictRow};
use std::rc::Rc;
use org_store::{JournalQuery, Store};
use org_sysfs::SysFs;
use tauri::{Manager, State};

/// Câte fișiere se parcurg cel mult la previzualizare, și câte se listează.
const MAX_SCAN: usize = 200_000;
const MAX_LIST: usize = 300;
/// Cât citește previzualizarea din conținutul fișierelor înainte să se oprească.
const PREVIEW_TEXT_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
/// Copiile regulilor: cel mult una la 10 minute, ultimele 30.
const BACKUP_GAP: std::time::Duration = std::time::Duration::from_secs(600);
const BACKUP_KEEP: usize = 30;

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

fn obj(v: Vec<(&str, Value)>) -> Value {
    Value::Obj(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn int(n: impl TryInto<i64>) -> Value {
    Value::Int(n.try_into().unwrap_or(i64::MAX))
}

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Decalajul local la un moment dat (ora de vară după data respectivă), ca în daemon.
fn offset_at(ts: i64) -> i64 {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map_or_else(|| chrono::Local::now().offset().local_minus_utc() as i64, |d| d.offset().local_minus_utc() as i64)
}

fn kind_and_dst(k: OpKind, dst: &Option<PathBuf>) -> Value {
    obj(vec![("kind", s(kind_name(k))), ("dst", dst.as_ref().map_or(Value::Null, |d| s(&d.to_string_lossy())))])
}

fn empty_doc() -> String {
    org_portable::export(&[], &Default::default(), true)
}

// ---------- regulile ascunse ----------

/// Deblocarea regulilor ascunse ține doar cât trăiește fereastra.
#[derive(Default)]
struct Gate(std::sync::Mutex<bool>);

impl Gate {
    fn unlocked(&self) -> bool {
        self.0.lock().map(|g| *g).unwrap_or(false)
    }
    fn set(&self, v: bool) {
        if let Ok(mut g) = self.0.lock() {
            *g = v;
        }
    }
}

/// `settings.json` ca listă de chei (limba, parola); cheile necunoscute se păstrează.
fn read_settings(files: &DataFiles) -> Vec<(String, Value)> {
    match fs::read_to_string(&files.settings).ok().and_then(|t| json::parse(&t).ok()) {
        Some(Value::Obj(o)) => o,
        _ => Vec::new(),
    }
}

fn write_settings(files: &DataFiles, key: &str, val: Option<Value>) -> Result<(), String> {
    let mut o = read_settings(files);
    o.retain(|(k, _)| k != key);
    if let Some(v) = val {
        o.push((key.to_string(), v));
    }
    let text = json::to_string_pretty(&Value::Obj(o));
    if fs::read_to_string(&files.settings).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    fs::create_dir_all(&files.dir).map_err(|e| e.to_string())?;
    let tmp = files.settings.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &files.settings).map_err(|e| e.to_string())
}

const PW_KEY: &str = "hidden_password";

/// Cât stă un fișier nemodificat înainte să fie procesat (citit și de daemon).
fn saved_settle(files: &DataFiles) -> i64 {
    match read_settings(files).into_iter().find(|(k, _)| k == "settle_secs") {
        Some((_, Value::Int(n))) => n.clamp(0, MAX_SETTLE),
        _ => 5,
    }
}

const MAX_SETTLE: i64 = 86_400;

/// Intervalul de așteptare dinainte de procesare; daemonul îl preia imediat.
#[tauri::command]
fn set_settle(secs: i64, files: State<'_, DataFiles>) -> Result<(), String> {
    if !(0..=MAX_SETTLE).contains(&secs) {
        return Err(format!("intervalul trebuie să fie între 0 și {MAX_SETTLE} secunde"));
    }
    write_settings(&files, "settle_secs", Some(int(secs)))
}

/// Câte zile se păstrează jurnalul (0 = tot); daemonul curăță la pornire și zilnic.
fn saved_journal_days(files: &DataFiles) -> i64 {
    match read_settings(files).into_iter().find(|(k, _)| k == "journal_days") {
        Some((_, Value::Int(n))) => n.clamp(0, 36_500),
        _ => 90,
    }
}

#[tauri::command]
fn set_journal_days(days: i64, files: State<'_, DataFiles>) -> Result<(), String> {
    if !(0..=36_500).contains(&days) {
        return Err("numărul de zile trebuie să fie între 0 și 36500".into());
    }
    write_settings(&files, "journal_days", Some(int(days)))
}

fn stored_password(files: &DataFiles) -> Option<PasswordHash> {
    let o = read_settings(files);
    let Some((_, Value::Obj(p))) = o.iter().find(|(k, _)| k == PW_KEY) else { return None };
    let get = |k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| v);
    match (get("salt"), get("iter"), get("hash")) {
        (Some(Value::Str(salt)), Some(Value::Int(iter)), Some(Value::Str(hash))) => Some(PasswordHash {
            salt: salt.clone(),
            iter: u32::try_from(*iter).ok()?,
            hash: hash.clone(),
        }),
        _ => None,
    }
}

/// Regulile ascunse se ascund doar dacă există o parolă și fereastra nu e deblocată.
fn locked(files: &DataFiles, gate: &Gate) -> bool {
    !gate.unlocked() && stored_password(files).is_some()
}

/// Regulile de pe disc (fără eroare dacă fișierul lipsește).
fn disk_rules(files: &DataFiles) -> Result<org_portable::Bundle, String> {
    match fs::read_to_string(&files.rules) {
        Ok(t) => org_portable::import(&t).map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(org_portable::Bundle::default()),
        Err(e) => Err(e.to_string()),
    }
}

fn hidden_ids(files: &DataFiles) -> Vec<u64> {
    disk_rules(files).map(|b| b.rules.iter().filter(|r| r.hidden).map(|r| r.id).collect()).unwrap_or_default()
}

/// Starea regulilor ascunse pentru fereastră.
#[tauri::command]
fn gate(files: State<'_, DataFiles>, gate: State<'_, Gate>) -> String {
    let password = stored_password(&files).is_some();
    json::to_string_pretty(&obj(vec![
        ("password", Value::Bool(password)),
        ("unlocked", Value::Bool(!password || gate.unlocked())),
        ("hidden", int(hidden_ids(&files).len())),
    ]))
}

#[tauri::command]
async fn unlock(password: String, files: State<'_, DataFiles>, gate: State<'_, Gate>) -> Result<bool, String> {
    let Some(ph) = stored_password(&files) else { return Ok(true) };
    let ok = ph.verify(&password);
    if ok {
        gate.set(true);
    } else {
        // Încercările greșite se plătesc cu timp.
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    Ok(ok)
}

#[tauri::command]
fn lock(gate: State<'_, Gate>) {
    gate.set(false);
}

/// Pune, schimbă (`old` = parola veche) sau scoate (`new` = nimic) parola.
#[tauri::command]
async fn set_hidden_password(
    old: Option<String>,
    new: Option<String>,
    files: State<'_, DataFiles>,
    gate: State<'_, Gate>,
) -> Result<(), String> {
    if let Some(ph) = stored_password(&files) {
        if !ph.verify(old.as_deref().unwrap_or("")) {
            std::thread::sleep(std::time::Duration::from_millis(600));
            return Err("parola actuală nu e corectă".into());
        }
    }
    match new {
        Some(p) if p.chars().count() < 4 => Err("parola trebuie să aibă cel puțin 4 caractere".into()),
        Some(p) => {
            let ph = PasswordHash::new(&p, &new_salt());
            let v = obj(vec![("salt", s(&ph.salt)), ("iter", int(ph.iter)), ("hash", s(&ph.hash))]);
            write_settings(&files, PW_KEY, Some(v))?;
            gate.set(true);
            Ok(())
        }
        None => {
            write_settings(&files, PW_KEY, None)?;
            gate.set(false);
            Ok(())
        }
    }
}

/// Documentul de reguli așa cum e pe disc (sau unul gol, dacă nu există încă); cât
/// fereastra e blocată, fără regulile ascunse.
#[tauri::command]
fn get_rules(files: State<'_, DataFiles>, gate: State<'_, Gate>) -> Result<String, String> {
    if locked(&files, &gate) {
        let b = disk_rules(&files)?;
        return Ok(org_portable::export(&b.rules, &b.dictionaries, false));
    }
    match fs::read_to_string(&files.rules) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(empty_doc()),
        Err(e) => Err(e.to_string()),
    }
}

/// Validează și salvează documentul de reguli. Scrierea e atomică (fișier temporar, apoi
/// redenumire), ca daemonul să nu citească niciodată un fișier pe jumătate scris.
/// Regulile primesc aici id-ul permanent (redenumirea nu le mai face să proceseze din nou
/// fișierele); documentul normalizat, cu id-uri, se întoarce ferestrei.
#[tauri::command]
fn save_rules(text: String, files: State<'_, DataFiles>, gate: State<'_, Gate>) -> Result<String, String> {
    let b = org_portable::import(&text).map_err(|e| e.to_string())?;
    let is_locked = locked(&files, &gate);
    // Fereastra blocată nu are regulile ascunse: se iau de pe disc, pe locurile lor.
    let mut rules = if is_locked { org_portable::merge_hidden(b.rules, &disk_rules(&files)?.rules) } else { b.rules };
    org_portable::assign_ids(&mut rules);
    let out = org_portable::export(&rules, &b.dictionaries, true);
    let back = if is_locked { org_portable::export(&rules, &b.dictionaries, false) } else { out.clone() };
    fs::create_dir_all(&files.dir).map_err(|e| e.to_string())?;
    // Copie a versiunii de pe disc, ca o greșeală să se poată anula (Setări → Importă).
    if fs::read_to_string(&files.rules).ok().as_deref() != Some(out.as_str()) {
        let now = now_ts();
        let stamp = org_core::timefmt::format(now, offset_at(now), "AAAA-LL-ZZ_HH-MM-SS", Lang::Ro);
        if let Err(e) = backup_rules(&files.dir, &files.rules, &stamp, BACKUP_GAP, BACKUP_KEEP) {
            eprintln!("copia regulilor: {e}");
        }
    }
    let tmp = files.rules.with_extension("orgrules.tmp");
    fs::write(&tmp, &out).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &files.rules).map_err(|e| e.to_string())?;
    Ok(back)
}

/// Catalogul amprentelor, încărcat o dată din baza de date și ținut la zi cu ce calculează
/// fereastra (previzualizări, confirmări, pagina Dubluri). Daemonul îl scrie și el.
#[derive(Default)]
struct AppPrints(Mutex<Option<AppCatalog>>);

/// Amprentele și rezultatele condițiilor pe conținut, ca în daemon.
struct AppCatalog {
    prints: HashMap<PathBuf, PrintRow>,
    verdicts: HashMap<(PathBuf, u64), VerdictRow>,
}

fn load_prints(files: &DataFiles, ap: &AppPrints) -> Prints {
    let Ok(mut g) = ap.0.lock() else { return Prints::default() };
    let c = g.get_or_insert_with(|| {
        let st = Store::open(&files.db).ok();
        AppCatalog {
            prints: st.as_ref().and_then(|s| s.load_prints().ok()).unwrap_or_default().into_iter().collect(),
            verdicts: st.as_ref().and_then(|s| s.load_verdicts().ok()).unwrap_or_default().into_iter().collect(),
        }
    });
    Prints::from_rows(c.prints.iter().map(|(p, r)| (p.clone(), *r)))
        .with_verdicts(c.verdicts.iter().map(|(k, r)| (k.clone(), *r)))
}

fn save_prints(files: &DataFiles, ap: &AppPrints, prints: &Prints) {
    let changes = prints.take_changes();
    let verdicts = prints.take_verdict_changes();
    if changes.is_empty() && verdicts.is_empty() {
        return;
    }
    if let Ok(mut g) = ap.0.lock() {
        if let Some(c) = g.as_mut() {
            c.prints.extend(changes.iter().cloned());
            c.verdicts.extend(verdicts.iter().cloned());
        }
    }
    if let Ok(mut st) = Store::open(&files.db) {
        let _ = st.save_prints(&changes);
        let _ = st.save_verdicts(&verdicts);
    }
}

/// Previzualizarea în lucru. Fereastra întreabă cât a ajuns (`preview_progress`); o
/// previzualizare nouă (regula s-a schimbat) o oprește pe cea veche, iar „Oprește”
/// (`stop_preview`) încheie cu ce s-a găsit până atunci.
#[derive(Default)]
struct PreviewJob {
    gen: AtomicU64,
    stop: AtomicBool,
    progress: Mutex<PreviewProgress>,
}

#[derive(Debug, Clone, Default)]
struct PreviewProgress {
    gen: u64,
    /// `list` (se adună fișierele), `check` (se verifică), `done`.
    phase: &'static str,
    /// Fișierele găsite până acum; la verificare, totalul.
    total: usize,
    checked: usize,
    matched: usize,
    started: Option<std::time::Instant>,
}

impl PreviewJob {
    fn set(&self, f: impl FnOnce(&mut PreviewProgress)) {
        if let Ok(mut p) = self.progress.lock() {
            f(&mut p);
        }
    }
}

/// Cât a ajuns previzualizarea: fereastra arată bara, procentul și timpul rămas.
#[tauri::command]
fn preview_progress(job: State<'_, PreviewJob>) -> String {
    progress_json(&job)
}

fn progress_json(job: &PreviewJob) -> String {
    let p = job.progress.lock().map(|p| p.clone()).unwrap_or_default();
    json::to_string_pretty(&obj(vec![
        ("gen", int(p.gen)),
        ("phase", s(if p.phase.is_empty() { "done" } else { p.phase })),
        ("total", int(p.total)),
        ("checked", int(p.checked)),
        ("matched", int(p.matched)),
        ("elapsed_ms", int(p.started.map_or(0, |t| t.elapsed().as_millis() as i64))),
    ]))
}

/// Căutarea din pagina Dubluri (aceeași mecanică de progres și oprire ca previzualizarea).
#[derive(Default)]
struct DupJob(PreviewJob);

#[tauri::command]
fn dup_progress(job: State<'_, DupJob>) -> String {
    progress_json(&job.0)
}

#[tauri::command]
fn stop_dups(job: State<'_, DupJob>) {
    job.0.stop.store(true, Ordering::SeqCst);
}

/// Cât se întorc grupuri în fereastră (cele mai mari primele); totalurile le cuprind pe toate.
const MAX_GROUPS: usize = 1000;

/// Grupurile de fișiere identice din folderele date. `text`: `{"folders": [{path, recursive}],
/// "min_size": n}`. Primul fișier din fiecare grup e originalul (cel mai vechi).
#[tauri::command]
async fn find_duplicates(
    text: String,
    files: State<'_, DataFiles>,
    job: State<'_, DupJob>,
    ap: State<'_, AppPrints>,
) -> Result<String, String> {
    let v = json::parse(&text).map_err(|e| format!("{e:?}"))?;
    let Value::Obj(o) = &v else { return Err("cerere greșită".into()) };
    let get = |k: &str| o.iter().find(|(x, _)| x == k).map(|(_, y)| y);
    let folders: Vec<org_core::WatchedFolder> = match get("folders") {
        Some(Value::Arr(a)) => a
            .iter()
            .filter_map(|f| match f {
                Value::Obj(fo) => {
                    let path = fo.iter().find_map(|(k, v)| match (k.as_str(), v) { ("path", Value::Str(p)) => Some(p.clone()), _ => None })?;
                    let recursive = !fo.iter().any(|(k, v)| k == "recursive" && matches!(v, Value::Bool(false)));
                    (!path.trim().is_empty()).then(|| org_core::WatchedFolder { path: PathBuf::from(path), recursive })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if folders.is_empty() {
        return Err("niciun folder".into());
    }
    let min_size = match get("min_size") { Some(Value::Int(n)) => (*n).max(0) as u64, _ => 0 };

    let job = &job.0;
    let my = job.gen.fetch_add(1, Ordering::SeqCst) + 1;
    job.stop.store(false, Ordering::SeqCst);
    let started = std::time::Instant::now();
    job.set(|p| *p = PreviewProgress { gen: my, phase: "list", started: Some(started), ..Default::default() });

    let unavailable: Vec<Value> =
        folders.iter().filter(|f| SysFs.list_dir(&f.path).is_err()).map(|f| s(&f.path.to_string_lossy())).collect();
    let dirs = DirCache::new(Rc::new(load_prints(&files, &ap)));
    let mut stopped = false;
    let mut last = std::time::Instant::now();
    let found = dirs.groups(&SysFs, &folders, min_size, &mut |step, done, total| {
        if job.gen.load(Ordering::SeqCst) != my || job.stop.load(Ordering::SeqCst) {
            stopped = true;
            return false;
        }
        if last.elapsed() >= std::time::Duration::from_millis(80) {
            last = std::time::Instant::now();
            job.set(|p| {
                p.phase = match step {
                    org_engine::GroupStep::Listing => "list",
                    org_engine::GroupStep::Quick => "quick",
                    org_engine::GroupStep::Full => "full",
                };
                p.checked = done;
                p.total = total;
            });
        }
        true
    });
    // Amprentele calculate rămân, și dacă s-a oprit.
    save_prints(&files, &ap, dirs.prints());
    if job.gen.load(Ordering::SeqCst) != my {
        return Err("înlocuită de o căutare nouă".into());
    }
    job.set(|p| p.phase = "done");
    let groups = found.unwrap_or_default();
    let copies: usize = groups.iter().map(|g| g.files.len() - 1).sum();
    let wasted: u64 = groups.iter().map(|g| g.wasted()).sum();
    let list: Vec<Value> = groups
        .iter()
        .take(MAX_GROUPS)
        .map(|g| {
            obj(vec![
                ("size", int(g.size)),
                ("wasted", int(g.wasted())),
                (
                    "files",
                    Value::Arr(
                        g.files
                            .iter()
                            .map(|f| {
                                obj(vec![
                                    ("path", s(&f.path.to_string_lossy())),
                                    ("folder", f.path.parent().map_or(Value::Null, |d| s(&d.to_string_lossy()))),
                                    ("created", int(f.created)),
                                    ("modified", int(f.modified)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    Ok(json::to_string_pretty(&obj(vec![
        ("groups", Value::Arr(list)),
        ("total_groups", int(groups.len())),
        ("copies", int(copies)),
        ("wasted", int(wasted)),
        ("stopped", Value::Bool(stopped)),
        ("unavailable", Value::Arr(unavailable)),
        ("elapsed_ms", int(started.elapsed().as_millis() as i64)),
    ])))
}

/// Trimite în Coș copiile alese din pagina Dubluri. `text`: `[{"keep": cale, "remove": [căi]}]`.
/// Fiecare copie se compară întâi octet cu octet cu cea păstrată: dacă s-a schimbat
/// între timp, rămâne. Totul intră în jurnal (se poate restaura).
#[tauri::command]
async fn trash_duplicates(text: String, files: State<'_, DataFiles>) -> Result<String, String> {
    let v = json::parse(&text).map_err(|e| format!("{e:?}"))?;
    let Value::Arr(items) = v else { return Err("cerere greșită".into()) };
    let str_of = |o: &[(String, Value)], k: &str| {
        o.iter().find_map(|(x, v)| match (x == k, v) { (true, Value::Str(p)) => Some(PathBuf::from(p)), _ => None })
    };
    let mut store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let mut fs = SysFs;
    let (mut done, mut skipped, mut failed) = (0usize, Vec::new(), Vec::new());
    let mut freed = 0u64;
    let run_id = store.next_run_id();
    for it in items {
        let Value::Obj(o) = it else { continue };
        let Some(keep) = str_of(&o, "keep") else { continue };
        let remove: Vec<PathBuf> = match o.iter().find(|(k, _)| k == "remove") {
            Some((_, Value::Arr(a))) => a.iter().filter_map(|x| if let Value::Str(p) = x { Some(PathBuf::from(p)) } else { None }).collect(),
            _ => Vec::new(),
        };
        for r in remove {
            // Nu se șterge niciodată fișierul păstrat și nici ceva care nu mai e identic.
            if r == keep || !fs.same_content(&keep, &r).unwrap_or(false) {
                skipped.push(s(&r.to_string_lossy()));
                continue;
            }
            let Ok(stat) = fs.metadata(&r) else {
                skipped.push(s(&r.to_string_lossy()));
                continue;
            };
            let id = store.begin(NewEntry {
                run_id,
                rule_id: MANUAL_RULE,
                ts: now_ts(),
                kind: OpKind::Trash,
                src: r.clone(),
                dst: Some(keep.clone()),
                size: stat.size,
                mtime: stat.modified,
            });
            match fs.trash(&r) {
                Ok(()) => {
                    store.finish(id, Status::Done);
                    done += 1;
                    freed += stat.size;
                }
                Err(e) => {
                    store.finish(id, Status::Failed(e.to_string()));
                    failed.push(obj(vec![("path", s(&r.to_string_lossy())), ("error", s(&e.to_string()))]));
                }
            }
        }
    }
    Ok(json::to_string_pretty(&obj(vec![
        ("done", int(done)),
        ("freed", int(freed)),
        ("skipped", Value::Arr(skipped)),
        ("failed", Value::Arr(failed)),
    ])))
}

/// „Oprește”: previzualizarea curentă se încheie cu rezultatele de până acum.
#[tauri::command]
fn stop_preview(job: State<'_, PreviewJob>) {
    job.stop.store(true, Ordering::SeqCst);
}

/// Fișierele din folderele regulii care se potrivesc cu condițiile ei (maximum `MAX_LIST`).
/// `text` e un document `.orgrules` cu o singură regulă.
#[tauri::command]
async fn preview(
    text: String,
    lang: Option<String>,
    files: State<'_, DataFiles>,
    job: State<'_, PreviewJob>,
    ap: State<'_, AppPrints>,
) -> Result<String, String> {
    let bundle = org_portable::import(&text).map_err(|e| e.to_string())?;
    let dicts: HashMap<String, org_core::Dictionary> = bundle.dictionaries.into_iter().collect();
    let mut rules = bundle.rules;
    if rules.is_empty() {
        return Err("nicio regulă".into());
    }
    rules.truncate(1);
    org_portable::assign_ids(&mut rules);
    let rule = &rules[0];

    let my = job.gen.fetch_add(1, Ordering::SeqCst) + 1;
    job.stop.store(false, Ordering::SeqCst);
    let started = std::time::Instant::now();
    job.set(|p| *p = PreviewProgress { gen: my, phase: "list", started: Some(started), ..Default::default() });
    // A pornit între timp o previzualizare mai nouă: aceasta nu mai contează.
    let superseded = || job.gen.load(Ordering::SeqCst) != my;
    const REPLACED: &str = "înlocuită de o previzualizare nouă";

    // Doar starea acestei reguli (cheia primară începe cu rule_id), nu tot tabelul.
    // Și cele care așteaptă confirmarea (apar ca atare, nu „deja procesat”).
    let (processed, awaiting): (HashSet<PathBuf>, HashSet<PathBuf>) = match Store::open(&files.db) {
        Ok(st) => (
            st.processed_paths(rule.id).unwrap_or_default().into_iter().collect(),
            st.pending().into_iter().filter(|p| p.rule_id == rule.id).map(|p| p.path).collect(),
        ),
        Err(_) => (HashSet::new(), HashSet::new()),
    };

    let ctx = EvalCtx { now: now_ts() };
    let lang = if lang.as_deref() == Some("en") { Lang::En } else { Lang::Ro };
    let rctx = RenderCtx { now: ctx.now, tz: Tz::At(offset_at), lang, dicts: &dicts, content: None };
    let fs = SysFs;

    // 1. Lista fișierelor (rapid), ca progresul să aibă un total.
    let mut found: Vec<(org_fs::DirEntry, usize)> = Vec::new();
    let mut unavailable = Vec::new();
    let mut capped = false;
    'list: for (wi, wf) in rule.folders.iter().enumerate() {
        if fs.list_dir(&wf.path).is_err() {
            unavailable.push(s(&wf.path.to_string_lossy()));
            continue;
        }
        let mut stack = vec![wf.path.clone()];
        while let Some(dir) = stack.pop() {
            if superseded() {
                return Err(REPLACED.into());
            }
            let Ok(entries) = fs.list_dir(&dir) else { continue };
            for e in entries {
                if e.is_dir {
                    if wf.recursive {
                        stack.push(e.path);
                    }
                    continue;
                }
                if found.len() >= MAX_SCAN {
                    capped = true;
                    break 'list;
                }
                found.push((e, wi));
            }
            let n = found.len();
            job.set(|p| p.total = n);
        }
    }
    let total_files = found.len();
    job.set(|p| {
        p.phase = "check";
        p.total = total_files;
    });

    // 2. Verificarea, cu progres. Fișierele care așteaptă se trec „pe hârtie” prin acțiuni,
    // în ordine, ca daemonul: al doilea fișier cu același nume vede ținta ocupată de primul.
    let mut dry = DryFs::new(&fs);
    let (mut scanned, mut total) = (0usize, 0usize);
    let mut list = Vec::new();
    // Condițiile pe conținut citesc fișierele: previzualizarea se oprește după un timp.
    let reads_text = rule.condition.uses_content() || rule.renders_content(&dicts);
    let (mut slow, mut stopped) = (false, false);
    let text = org_engine::TextCache::default();
    let dirs = DirCache::new(Rc::new(load_prints(&files, &ap)));
    // Dublurile după amprente (catalog): afișare, nu acțiune.
    let content = FsContent::with_dirs(&fs, &text, Some(&dirs)).by_prints();
    let within = rule.condition.duplicate_within().unwrap_or(&[]);
    let mut last_report = std::time::Instant::now();
    for (e, wi) in &found {
        let wf = &rule.folders[*wi];
        if last_report.elapsed() >= std::time::Duration::from_millis(80) {
            last_report = std::time::Instant::now();
            if superseded() {
                return Err(REPLACED.into());
            }
            if job.stop.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            job.set(|p| {
                p.checked = scanned;
                p.matched = total;
            });
        }
        if reads_text && started.elapsed() > PREVIEW_TEXT_BUDGET {
            slow = true;
            break;
        }
        scanned += 1;
        if org_engine::is_temp_download(&e.path) {
            continue;
        }
        // Metadatele vin din listare (pe Windows fără citiri în plus).
        let Some((st, created)) = e.stat.or_else(|| fs.stat_full(&e.path).ok()) else { continue };
        let meta = FileMeta::new(&e.path, &wf.path, st.size, created, st.modified);
        if !rule.condition.matches(&Prepared::with_content(&meta, &content), &ctx) {
            continue;
        }
        total += 1;
        if list.len() >= MAX_LIST {
            continue;
        }
        let done = processed.contains(&e.path);
        let plan = if done {
            Value::Null
        } else {
            let with_text;
            let rc = if rule.renders_content(&dicts) {
                with_text = RenderCtx { content: content.load(&meta), ..rctx.clone() };
                &with_text
            } else {
                &rctx
            };
            let p = plan_rule(&mut dry, rule, &meta, rc);
            let moved = !p.gone && p.path != e.path;
            obj(vec![
                ("ops", Value::Arr(p.ops.iter().map(|(k, d)| kind_and_dst(*k, d)).collect())),
                ("dest", if moved { s(&p.path.to_string_lossy()) } else { Value::Null }),
                ("gone", Value::Bool(p.gone)),
                ("skipped", Value::Bool(p.skipped)),
                ("error", p.error.as_deref().map_or(Value::Null, s)),
                ("awaiting", Value::Bool(p.awaiting)),
                ("duplicate", Value::Bool(p.duplicate)),
            ])
        };
        // Pentru regulile cu dubluri: originalul, ca să se vadă ce rămâne.
        let original = if rule.condition.uses_duplicate() {
            content.duplicate_of(&meta, within).map_or(Value::Null, |o| s(&o.to_string_lossy()))
        } else {
            Value::Null
        };
        list.push(obj(vec![
            ("name", s(&meta.file_name())),
            ("folder", s(&meta.folder())),
            ("path", s(&meta.full_path())),
            ("size", int(st.size)),
            ("modified", int(st.modified)),
            ("created", int(created)),
            ("processed", Value::Bool(done)),
            ("pending", Value::Bool(awaiting.contains(&e.path))),
            ("plan", plan),
            ("duplicate_of", original),
        ]));
    }
    save_prints(&files, &ap, dirs.prints());
    if !superseded() {
        job.set(|p| {
            p.phase = "done";
            p.checked = scanned;
            p.matched = total;
        });
    }
    Ok(json::to_string_pretty(&obj(vec![
        ("scanned", int(scanned)),
        // Fișierele găsite în foldere (`scanned` < `found` dacă s-a oprit înainte).
        ("found", int(total_files)),
        ("capped", Value::Bool(capped)),
        ("matched", int(total)),
        ("truncated", Value::Bool(total > list.len())),
        ("slow", Value::Bool(slow)),
        ("stopped", Value::Bool(stopped)),
        ("elapsed_ms", int(started.elapsed().as_millis() as i64)),
        ("files", Value::Arr(list)),
        ("unavailable", Value::Arr(unavailable)),
    ])))
}

fn kind_name(k: OpKind) -> &'static str {
    match k {
        OpKind::Move => "move",
        OpKind::Copy => "copy",
        OpKind::Rename => "rename",
        OpKind::ChangeExt => "change_ext",
        OpKind::Trash => "trash",
        OpKind::Delete => "delete",
        OpKind::Run => "run",
        OpKind::Extract => "extract",
    }
}

fn entry_json(e: &Entry, names: &HashMap<u64, String>) -> Value {
    let (status, message) = match &e.status {
        Status::InProgress => ("in_progress", String::new()),
        Status::Done => ("done", String::new()),
        Status::Failed(m) => ("failed", m.clone()),
        Status::Undone => ("undone", String::new()),
        Status::Simulated => ("simulated", String::new()),
    };
    obj(vec![
        ("id", int(e.id)),
        ("run", int(e.run_id)),
        ("rule", names.get(&e.rule_id).map_or(Value::Null, |n| s(n))),
        ("rule_id", s(&e.rule_id.to_string())),
        ("ts", int(e.ts)),
        ("kind", s(kind_name(e.kind))),
        ("src", s(&e.src.to_string_lossy())),
        ("dst", e.dst.as_ref().map_or(Value::Null, |d| s(&d.to_string_lossy()))),
        ("status", s(status)),
        ("message", s(&message)),
    ])
}

/// Numele regulilor după id, ca jurnalul să arate numele și nu numere.
fn rule_names(files: &DataFiles) -> HashMap<u64, String> {
    let mut rules = fs::read_to_string(&files.rules)
        .ok()
        .and_then(|t| org_portable::import(&t).ok())
        .map(|b| b.rules)
        .unwrap_or_default();
    org_portable::assign_ids(&mut rules);
    let mut names: HashMap<u64, String> = rules.into_iter().map(|r| (r.id, r.name)).collect();
    names.insert(MANUAL_RULE, manual_name(files).into());
    names
}

/// Operațiile făcute din pagina Dubluri apar în jurnal sub această „regulă”.
const MANUAL_RULE: u64 = u64::MAX;

fn manual_name(files: &DataFiles) -> &'static str {
    let en = read_settings(files).iter().any(|(k, v)| k == "lang" && matches!(v, Value::Str(x) if x == "en"));
    if en { "Duplicates (manual)" } else { "Dubluri (manual)" }
}

/// Tipul acțiunilor (pentru „ce urmează după confirmare”).
fn action_kind(a: &org_core::Action) -> &'static str {
    use org_core::Action::*;
    match a {
        Move { .. } => "move",
        Copy { .. } => "copy",
        Rename { .. } => "rename",
        ChangeExtension { .. } => "change_extension",
        Trash { .. } => "trash",
        Delete { .. } => "delete",
        Run { .. } => "run",
        Command { .. } => "command",
        Confirm => "confirm",
        Extract { .. } => "extract",
    }
}

/// Originalul fiecărei dubluri care așteaptă confirmare, după (cale, dimensiune, dată):
/// fereastra întreabă la câteva secunde, iar căutarea compară conținutul de pe disc.
#[derive(Default)]
struct DupMemo(Mutex<HashMap<(PathBuf, u64, i64), Option<String>>>);

/// Confirmările care așteaptă răspuns (fără cele ale regulilor ascunse cât e blocat), pe
/// pagini: `total`, toate id-urile (pentru „Aprobă toate”) și detaliile paginii cerute.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn get_pending(
    limit: Option<u32>,
    offset: Option<u32>,
    sort: Option<String>,
    desc: Option<bool>,
    files: State<'_, DataFiles>,
    gate: State<'_, Gate>,
    memo: State<'_, DupMemo>,
    ap: State<'_, AppPrints>,
) -> Result<String, String> {
    let store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let bundle = disk_rules(&files)?;
    let hide = locked(&files, &gate);
    let mut all: Vec<_> = store
        .pending()
        .into_iter()
        .filter(|p| !(hide && bundle.rules.iter().any(|r| r.id == p.rule_id && r.hidden)))
        .collect();
    // Ordinea paginilor: după dată (implicit), nume, dimensiune, folder sau regulă.
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let name = |p: &org_journal::Pending| p.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let rule_name = |id: u64| bundle.rules.iter().find(|r| r.id == id).map(|r| r.name.to_lowercase()).unwrap_or_default();
    match sort.as_deref() {
        Some("name") => all.sort_by(|a, b| name(a).cmp(&name(b)).then(a.id.cmp(&b.id))),
        Some("size") => all.sort_by(|a, b| a.size.cmp(&b.size).then(a.id.cmp(&b.id))),
        Some("folder") => all.sort_by(|a, b| {
            let (fa, fb) = (a.path.parent().map(lower).unwrap_or_default(), b.path.parent().map(lower).unwrap_or_default());
            fa.cmp(&fb).then(name(a).cmp(&name(b)))
        }),
        Some("rule") => all.sort_by(|a, b| rule_name(a.rule_id).cmp(&rule_name(b.rule_id)).then(a.id.cmp(&b.id))),
        _ => all.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id))),
    }
    if desc == Some(true) {
        all.reverse();
    }
    let (limit, offset) = (limit.unwrap_or(50).clamp(1, 500) as usize, offset.unwrap_or(0) as usize);
    let text = org_engine::TextCache::default();
    let dirs = DirCache::new(Rc::new(load_prints(&files, &ap)));
    let items: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|p| {
            let rule = bundle.rules.iter().find(|r| r.id == p.rule_id);
            let st = SysFs.stat_full(&p.path).ok();
            let next: Vec<Value> =
                rule.map_or(Vec::new(), |r| r.actions.iter().skip(p.step).take_while(|a| !matches!(a, org_core::Action::Confirm)).map(|a| s(action_kind(a))).collect());
            // Pentru dubluri: originalul care rămâne (calculat o dată pe versiunea fișierului).
            let original = match (rule, st) {
                (Some(r), Some((m, created))) if r.condition.uses_duplicate() => {
                    let key = (p.path.clone(), m.size, m.modified);
                    let known = memo.0.lock().ok().and_then(|g| g.get(&key).cloned());
                    let found = known.unwrap_or_else(|| {
                        let meta = org_core::FileMeta::new(&p.path, &p.root, m.size, created, m.modified);
                        let within = r.condition.duplicate_within().unwrap_or(&[]);
                        let o = FsContent::with_dirs(&SysFs, &text, Some(&dirs))
                            .by_prints()
                            .duplicate_of(&meta, within)
                            .map(|o| o.to_string_lossy().into_owned());
                        if let Ok(mut g) = memo.0.lock() {
                            if g.len() > 20_000 {
                                g.clear();
                            }
                            g.insert(key, o.clone());
                        }
                        o
                    });
                    found.map_or(Value::Null, |o| s(&o))
                }
                _ => Value::Null,
            };
            obj(vec![
                ("id", s(&p.id.to_string())),
                ("rule_id", s(&p.rule_id.to_string())),
                ("rule", rule.map_or(Value::Null, |r| s(&r.name))),
                ("path", s(&p.path.to_string_lossy())),
                ("folder", p.path.parent().map_or(Value::Null, |f| s(&f.to_string_lossy()))),
                ("size", int(p.size)),
                ("ts", int(p.ts)),
                // Șters sau schimbat între timp: aprobarea îl reevaluează.
                ("exists", Value::Bool(st.is_some())),
                ("next", Value::Arr(next)),
                ("duplicate_of", original),
            ])
        })
        .collect();
    save_prints(&files, &ap, dirs.prints());
    Ok(json::to_string_pretty(&obj(vec![
        ("total", int(all.len())),
        ("ids", Value::Arr(all.iter().map(|p| s(&p.id.to_string())).collect())),
        ("offset", int(offset)),
        ("items", Value::Arr(items)),
    ])))
}

/// Răspunsul la confirmări; daemonul rulează (sau nu) pașii rămași.
#[tauri::command]
fn answer_pending(ids: Vec<String>, approve: bool, files: State<'_, DataFiles>) -> Result<(), String> {
    let ids = ids.iter().map(|i| parse_rule_id(i)).collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let req = if approve { Request::Approve { ids } } else { Request::Reject { ids } };
    write_request(&files.dir, &request_to_json(&req)).map(|_| ()).map_err(|e| e.to_string())
}

/// Id-urile regulilor vin din JavaScript ca text (numere pe 64 de biți).
fn parse_rule_id(rule: &str) -> Result<u64, String> {
    rule.trim().parse::<u64>().ok().filter(|&r| r != 0).ok_or_else(|| format!("id de regulă invalid: {rule}"))
}

/// Jurnalul, filtrat și pe pagini. `total` = toate intrările, `matched` = cele care se potrivesc.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn get_journal(
    limit: u32,
    offset: Option<u32>,
    text: Option<String>,
    rule: Option<String>,
    status: Option<String>,
    since: Option<i64>,
    until: Option<i64>,
    files: State<'_, DataFiles>,
    gate: State<'_, Gate>,
) -> Result<String, String> {
    let store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let exclude_rules = if locked(&files, &gate) { hidden_ids(&files) } else { Vec::new() };
    let names = rule_names(&files);
    let q = JournalQuery {
        text,
        rule: rule.as_deref().filter(|r| !r.is_empty()).map(parse_rule_id).transpose()?,
        status: status.filter(|s| !s.is_empty()),
        since,
        until,
        limit: limit.clamp(1, 5000),
        offset: offset.unwrap_or(0),
        exclude_rules,
    };
    let (entries, matched) = store.search(&q);
    let list: Vec<Value> = entries.iter().map(|e| entry_json(e, &names)).collect();
    Ok(json::to_string_pretty(&obj(vec![
        ("total", int(store.count())),
        ("matched", int(matched)),
        ("entries", Value::Arr(list)),
    ])))
}

/// Cifrele paginii Acasă. `bounds` sunt începuturile zilelor (ora locală, calculate de
/// fereastră), în ordine, plus sfârșitul ultimei zile: o zi = `[bounds[i], bounds[i+1])`.
#[tauri::command]
async fn get_stats(bounds: Vec<i64>, files: State<'_, DataFiles>, gate: State<'_, Gate>) -> Result<String, String> {
    if bounds.len() < 2 || bounds.len() > 400 || bounds.windows(2).any(|w| w[0] >= w[1]) {
        return Err("intervale greșite".into());
    }
    let store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let exclude = if locked(&files, &gate) { hidden_ids(&files) } else { Vec::new() };
    let names = rule_names(&files);
    let days: Vec<Value> = bounds
        .windows(2)
        .map(|w| {
            let st = store.period_stats(w[0], w[1], &exclude);
            obj(vec![
                ("done", int(st.done)),
                ("failed", int(st.failed)),
                ("simulated", int(st.simulated)),
                ("undone", int(st.undone)),
            ])
        })
        .collect();
    let top: Vec<Value> = store
        .top_rules(bounds[0], &exclude, 5)
        .into_iter()
        .map(|(id, n)| {
            obj(vec![
                ("rule_id", s(&id.to_string())),
                ("rule", names.get(&id).map_or(Value::Null, |n| s(n))),
                ("files", int(n)),
            ])
        })
        .collect();
    Ok(json::to_string_pretty(&obj(vec![("days", Value::Arr(days)), ("top", Value::Arr(top))])))
}

/// Fișierele vor fi procesate din nou de regulă (și după o restaurare). Cererea ajunge la
/// daemon ca fișier; dacă daemonul e oprit, o aplică la pornire.
#[tauri::command]
fn mark_pending(rule: String, paths: Vec<String>, files: State<'_, DataFiles>) -> Result<(), String> {
    let rule = parse_rule_id(&rule)?;
    if paths.is_empty() {
        return Ok(());
    }
    let req = Request::Pending { rule, paths: paths.into_iter().map(PathBuf::from).collect() };
    write_request(&files.dir, &request_to_json(&req)).map(|_| ()).map_err(|e| e.to_string())
}

/// Toate fișierele regulii vor fi procesate din nou.
#[tauri::command]
fn mark_rule_pending(rule: String, files: State<'_, DataFiles>) -> Result<(), String> {
    let req = Request::PendingRule { rule: parse_rule_id(&rule)? };
    write_request(&files.dir, &request_to_json(&req)).map(|_| ()).map_err(|e| e.to_string())
}

/// Pauză până la `until` (secunde Unix), `0` = până la reluare, fără valoare = reia.
#[tauri::command]
fn set_pause(until: Option<i64>, files: State<'_, DataFiles>) -> Result<(), String> {
    let path = files.dir.join(PAUSE_FILE);
    match until {
        Some(t) if t == 0 || t > now_ts() => fs::write(&path, t.to_string()).map_err(|e| e.to_string()),
        Some(_) => Err("momentul pauzei a trecut deja".into()),
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
    }
}

/// Arată fișierul în Explorer (selectat), deschide un folder; dacă fișierul nu mai există, deschide folderul lui.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    let mut cmd = std::process::Command::new("explorer.exe");
    if p.is_dir() {
        cmd.arg(&p); // un folder se deschide
    } else if p.exists() {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Explorer vrea `/select,"cale"` ca un singur argument, cu ghilimelele lui.
            cmd.raw_arg(format!("/select,\"{}\"", p.display()));
        }
        #[cfg(not(windows))]
        cmd.arg(&p);
    } else {
        let dir = p.ancestors().skip(1).find(|d| d.is_dir()).ok_or_else(|| format!("nu mai există: {path}"))?;
        cmd.arg(dir);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn outcome_json(id: u64, r: io::Result<UndoOutcome>) -> Value {
    let (result, detail) = match r {
        Ok(UndoOutcome::Restored(p)) => ("restored", p.to_string_lossy().into_owned()),
        Ok(UndoOutcome::NeedsConfirmation(w)) => ("needs_confirmation", w.to_string()),
        Ok(UndoOutcome::Unrecoverable(w)) => ("unrecoverable", w.to_string()),
        Ok(UndoOutcome::NotUndoable) => ("not_undoable", String::new()),
        Err(e) => ("error", e.to_string()),
    };
    obj(vec![("id", int(id)), ("result", s(result)), ("detail", s(&detail))])
}

/// Restaurează o operație din jurnal.
#[tauri::command]
async fn undo_entry(id: u64, force: bool, files: State<'_, DataFiles>) -> Result<String, String> {
    let mut store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let mut fs = SysFs;
    let r = undo(&mut fs, &mut store, id, force);
    Ok(json::to_string_pretty(&outcome_json(id, r)))
}

/// Restaurează toate operațiile unei rulări, în ordine inversă.
#[tauri::command]
async fn undo_whole_run(run: u64, force: bool, files: State<'_, DataFiles>) -> Result<String, String> {
    let mut store = Store::open(&files.db).map_err(|e| e.to_string())?;
    let mut fs = SysFs;
    let results: Vec<Value> =
        undo_run(&mut fs, &mut store, run, force).into_iter().map(|(id, r)| outcome_json(id, r)).collect();
    Ok(json::to_string_pretty(&Value::Arr(results)))
}

/// Citește un fișier `.orgrules` ales de utilizator, îl validează și îl întoarce normalizat.
#[tauri::command]
fn read_rules_file(path: String) -> Result<String, String> {
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let b = org_portable::import(&text).map_err(|e| e.to_string())?;
    Ok(org_portable::export(&b.rules, &b.dictionaries, true))
}

/// Exportă documentul curent într-un fișier ales de utilizator. Regulile ascunse nu se exportă.
#[tauri::command]
fn export_rules(path: String, text: String) -> Result<(), String> {
    let b = org_portable::import(&text).map_err(|e| e.to_string())?;
    fs::write(path, org_portable::export(&b.rules, &b.dictionaries, false)).map_err(|e| e.to_string())
}

/// Pauza din fișier: `Some(0)` = până la reluare; una expirată nu mai contează.
fn read_pause(files: &DataFiles) -> Option<i64> {
    let v: i64 = fs::read_to_string(files.dir.join(PAUSE_FILE)).ok()?.trim().parse().ok()?;
    (v == 0 || v > now_ts()).then_some(v)
}

#[tauri::command]
fn status(files: State<'_, DataFiles>) -> String {
    let running = daemon_running(&files.lock);
    // Starea scrisă de daemon (reguli, foldere indisponibile, numărători, ultima eroare);
    // fără daemon pornit poate fi veche, deci nu se trimite.
    let daemon = running
        .then(|| fs::read_to_string(files.dir.join(STATUS_FILE)).ok())
        .flatten()
        .and_then(|t| json::parse(&t).ok())
        .unwrap_or(Value::Null);
    json::to_string_pretty(&obj(vec![
        ("daemon_running", Value::Bool(running)),
        ("daemon", daemon),
        ("paused_until", read_pause(&files).map_or(Value::Null, Value::Int)),
        ("settle_secs", int(saved_settle(&files))),
        ("journal_days", int(saved_journal_days(&files))),
        ("now", int(now_ts())),
        ("data_dir", s(&files.dir.to_string_lossy())),
        ("rules_path", s(&files.rules.to_string_lossy())),
        ("backup_dir", s(&files.dir.join(BACKUP_DIR).to_string_lossy())),
        ("backups", int(rule_backups(&files.dir).len())),
        ("db_path", s(&files.db.to_string_lossy())),
        ("version", s(env!("CARGO_PKG_VERSION"))),
    ]))
}

/// Calea daemonului: `filepilot.exe`, lângă executabilul ferestrei.
fn daemon_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let daemon = exe.with_file_name(if cfg!(windows) { "filepilot.exe" } else { "filepilot" });
    if daemon.exists() {
        Ok(daemon)
    } else {
        Err(format!("nu găsesc {}", daemon.display()))
    }
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FilePilot";
/// The autostart value under the app's former name (see `migrate_autostart`).
const LEGACY_RUN_VALUE: &str = "Organizator";

fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new("reg");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.output()
}

/// Daemonul pornește odată cu Windows-ul dacă există valoarea în cheia Run a utilizatorului curent.
#[tauri::command]
fn get_autostart() -> bool {
    reg(&["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false)
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let out = if enabled {
        let value = format!("\"{}\"", daemon_path()?.display());
        reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])
    } else {
        reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
    }
    .map_err(|e| e.to_string())?;
    // La dezactivare, lipsa valorii e deja starea dorită.
    if out.status.success() || (!enabled && !get_autostart()) {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Autostart set up under the former name points to `organizator.exe`, which no longer
/// exists after the rename: it is moved to the new value, pointing to the current daemon.
/// (The installer does the same; this covers copies run without it.)
fn migrate_autostart() {
    let legacy = reg(&["query", RUN_KEY, "/v", LEGACY_RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false);
    if !legacy {
        return;
    }
    if set_autostart(true).is_ok() {
        let _ = reg(&["delete", RUN_KEY, "/v", LEGACY_RUN_VALUE, "/f"]);
    }
}

/// Limba ferestrei, salvată și pentru daemon (meniul pictogramei, la următoarea pornire).
#[tauri::command]
fn set_language(lang: String, files: State<'_, DataFiles>) -> Result<(), String> {
    if lang != "ro" && lang != "en" {
        return Err(format!("limbă necunoscută: {lang}"));
    }
    write_settings(&files, "lang", Some(s(&lang)))
}

/// Pornește daemonul (`filepilot.exe`, lângă executabilul ferestrei), fără fereastră de consolă.
#[tauri::command]
fn start_daemon(files: State<'_, DataFiles>) -> Result<(), String> {
    if daemon_running(&files.lock) {
        return Ok(());
    }
    let daemon = daemon_path()?;
    let mut cmd = std::process::Command::new(daemon);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | DETACHED_PROCESS: rămâne în viață și după închiderea ferestrei.
        cmd.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    let files = DataFiles::default_ensured().expect("nu pot crea folderul de date");
    if cfg!(windows) {
        migrate_autostart();
    }
    tauri::Builder::default()
        // Prima: o a doua pornire (ex. din tray) doar aduce în față fereastra deja deschisă.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        // Ține minte poziția, dimensiunea și starea maximizată a ferestrei între porniri.
        .plugin(tauri_plugin_window_state::Builder::default()
            .with_state_flags(tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE)
            .build())
        .plugin(tauri_plugin_dialog::init())
        .manage(files)
        .manage(Gate::default())
        .manage(PreviewJob::default())
        .manage(DupMemo::default())
        .manage(AppPrints::default())
        .manage(DupJob::default())
        // Fereastra pornește ascunsă; pagina o arată după primul desen, deja la poziția salvată.
        // Plasă de siguranță: dacă pagina nu apucă s-o arate, apare după 5 secunde.
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let _ = w.show();
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_rules,
            save_rules,
            preview,
            preview_progress,
            find_duplicates,
            dup_progress,
            stop_dups,
            trash_duplicates,
            stop_preview,
            get_journal,
            get_stats,
            undo_entry,
            undo_whole_run,
            read_rules_file,
            export_rules,
            status,
            start_daemon,
            get_autostart,
            set_autostart,
            set_language,
            mark_pending,
            mark_rule_pending,
            set_pause,
            reveal,
            gate,
            unlock,
            lock,
            set_hidden_password,
            set_settle,
            get_pending,
            answer_pending,
            set_journal_days
        ])
        .run(tauri::generate_context!())
        .expect("eroare la pornirea ferestrei");
}
