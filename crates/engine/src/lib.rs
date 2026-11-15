//! Motorul: leagă regulile de fișiere. Parcurge folderele monitorizate, așteaptă ca
//! fișierele să fie stabile (descărcarea s-a terminat), ține minte ce a procesat și
//! rulează regulile în ordine.
//!
//! Nu face I/O direct: totul trece prin `Fs`. Evenimentele de la sistemul de operare
//! (watcher-ul) doar cheamă `process_path`; scanarea completă (`scan`) rulează la pornire.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

mod dups;
pub use dups::{DirCache, DupFile, DupGroup, GroupStep, PrintRow, Prints, VerdictRow};

use org_actions::{run_rule, run_rule_from, RunReport, StepStatus};
use org_core::{Action, ContentLoader, EvalCtx, FileMeta, RenderCtx, Rule, WatchedFolder};
use org_fs::{DryFs, FileStat, Fs};
use org_journal::{Journal, Pending};

/// Pauzele dintre reîncercări după un eșec (secunde): 1 min, 5 min, 15 min, 1 h.
/// După ultima, fișierul rămâne marcat procesat (se poate marca „în așteptare”).
const RETRY_BACKOFF: &[i64] = &[60, 300, 900, 3600];
/// La câte secunde se reverifică un fișier deschis de alt program.
pub const IN_USE_RECHECK: i64 = 3;

/// Extensii pe care browserele le folosesc cât timp descarcă: nu se ating niciodată.
/// Fișiere încă în descărcare: Chrome/Edge (`.crdownload`), Firefox (`.part`), Opera,
/// Safari, IE (`.partial`), qBittorrent (`.!qB`), µTorrent (`.!ut`), aria2 (`.aria2`).
const TEMP_EXTENSIONS: &[&str] =
    &["crdownload", "part", "partial", "tmp", "download", "opdownload", "!qb", "!ut", "aria2"];

/// Un fișier cu „frate” temporar (`raport.pdf` lângă `raport.pdf.part`) e locțiitorul unei
/// descărcări: Firefox creează `raport.pdf` gol și îl înlocuiește la final; aria2 ține
/// `raport.pdf.aria2` cât descarcă.
const TEMP_SIBLINGS: &[&str] = &["part", "crdownload", "download", "opdownload", "partial", "aria2", "!qB"];

fn download_in_progress(fs: &dyn Fs, path: &Path) -> bool {
    TEMP_SIBLINGS.iter().any(|ext| {
        let mut s = path.as_os_str().to_owned();
        s.push(".");
        s.push(ext);
        fs.exists(Path::new(&s))
    })
}

pub fn is_temp_download(path: &Path) -> bool {
    // A folder FilePilot is still extracting an archive into counts as a download in progress.
    org_fs::in_part_folder(path)
        || path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|e| TEMP_EXTENSIONS.contains(&e.as_str()))
}

/// Un fișier e stabil când dimensiunea și data modificării nu s-au schimbat de cel
/// puțin `settle` secunde între două observații.
#[derive(Debug, Default)]
pub struct StabilityTracker {
    seen: HashMap<PathBuf, (FileStat, i64)>,
}

impl StabilityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_stable(&mut self, path: &Path, stat: FileStat, now: i64, settle: i64) -> bool {
        match self.seen.get(path) {
            Some((old, since)) if *old == stat => now - *since >= settle,
            _ => {
                self.seen.insert(path.to_path_buf(), (stat, now));
                settle <= 0
            }
        }
    }

    /// Câte secunde mai are de așteptat un fișier urmărit (0 = gata sau neurmărit).
    pub fn remaining(&self, path: &Path, now: i64, settle: i64) -> i64 {
        self.seen.get(path).map_or(0, |(_, since)| (since + settle - now).max(0))
    }

    pub fn forget(&mut self, path: &Path) {
        self.seen.remove(path);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// O schimbare a stării fișierelor, de salvat în baza de date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateChange {
    Set(u64, PathBuf, FileStat),
    ClearPath(PathBuf),
    /// Un fișier, pentru o singură regulă.
    Clear(u64, PathBuf),
    /// Toate fișierele unei reguli.
    ClearRule(u64),
}

/// Ce a procesat fiecare regulă: (regulă, cale) → starea fișierului la acel moment.
/// Un fișier se reprocesează doar dacă s-a modificat sau a fost marcat „în așteptare”.
/// Fișierele care NU se potrivesc nu se memorează: condițiile pe metadate costă
/// nanosecunde, iar așa o regulă editată sau o condiție „mai vechi de N zile” se aplică
/// și fișierelor deja existente.
/// Schimbările se țin într-un jurnal intern, ca să poată fi salvate (`take_changes`).
#[derive(Debug, Default)]
pub struct FileState {
    done: HashMap<(u64, PathBuf), FileStat>,
    changes: Vec<StateChange>,
}

impl FileState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Încarcă o intrare salvată, fără a o înregistra ca schimbare.
    pub fn load(&mut self, rule: u64, path: PathBuf, stat: FileStat) {
        self.done.insert((rule, path), stat);
    }

    /// Schimbările de la ultimul apel, în ordine.
    pub fn take_changes(&mut self) -> Vec<StateChange> {
        std::mem::take(&mut self.changes)
    }

    pub fn is_done(&self, rule: u64, path: &Path, stat: FileStat) -> bool {
        self.done.get(&(rule, path.to_path_buf())) == Some(&stat)
    }

    pub fn mark_done(&mut self, rule: u64, path: &Path, stat: FileStat) {
        let key = (rule, path.to_path_buf());
        if self.done.get(&key) != Some(&stat) {
            self.changes.push(StateChange::Set(rule, key.1.clone(), stat));
            self.done.insert(key, stat);
        }
    }

    /// „În așteptare”: fișierul va fi procesat din nou de toate regulile.
    pub fn mark_pending(&mut self, path: &Path) {
        let before = self.done.len();
        self.done.retain(|(_, p), _| p != path);
        if self.done.len() != before {
            self.changes.push(StateChange::ClearPath(path.to_path_buf()));
        }
    }

    /// Fișierul va fi procesat din nou de o regulă.
    pub fn forget(&mut self, rule: u64, path: &Path) {
        if self.done.remove(&(rule, path.to_path_buf())).is_some() {
            self.changes.push(StateChange::Clear(rule, path.to_path_buf()));
        }
    }

    /// Toate fișierele unei reguli vor fi procesate din nou.
    pub fn forget_rule(&mut self, rule: u64) {
        let before = self.done.len();
        self.done.retain(|(r, _), _| *r != rule);
        if self.done.len() != before {
            self.changes.push(StateChange::ClearRule(rule));
        }
    }

    pub fn len(&self) -> usize {
        self.done.len()
    }

    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
    }
}

pub struct Engine {
    rules: Vec<Rule>,
    pub state: FileState,
    pub stability: StabilityTracker,
    /// Secunde de liniște înainte ca un fișier să fie considerat gata.
    pub settle_secs: i64,
    /// Prefixe de cale (în minuscule) care nu se ating niciodată: baza de date, regulile și
    /// lacătul programului, dacă se află într-un folder monitorizat.
    exclude: Vec<String>,
    /// Eșecuri temporare: (regulă, cale) → reîncercarea programată.
    failures: HashMap<(u64, PathBuf), Retry>,
    /// Fișiere care așteaptă o confirmare: nicio regulă nu le atinge până la răspuns.
    awaiting: std::collections::HashSet<PathBuf>,
    /// Catalogul amprentelor pentru dubluri (daemonul îl încarcă și îl salvează).
    prints: Rc<Prints>,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    attempts: usize,
    /// De când are voie regula să încerce din nou.
    due: i64,
    /// Când se predă calea daemonului (la `due`, apoi din oră în oră dacă nu o mai
    /// procesează nimic, de pildă pentru că regula a fost oprită).
    offer: i64,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub seen: usize,
    pub processed: Vec<RunReport>,
    /// Fișiere care încă se modifică; se reiau la următoarea trecere.
    pub waiting: usize,
    /// Care sunt și când să fie reluate: (cale, peste câte secunde).
    pub waiting_paths: Vec<(PathBuf, i64)>,
}

impl Engine {
    /// Peste câte secunde (cel puțin 1) poate deveni stabil un fișier care așteaptă.
    pub fn settle_left(&self, path: &Path, now: i64) -> i64 {
        self.stability.remaining(path, now, self.settle_secs).max(1)
    }

    pub fn new(rules: Vec<Rule>, settle_secs: i64) -> Self {
        Engine {
            rules: rules.into_iter().map(Rule::optimized).collect(),
            state: FileState::new(),
            stability: StabilityTracker::new(),
            settle_secs,
            exclude: Vec::new(),
            failures: HashMap::new(),
            awaiting: std::collections::HashSet::new(),
            prints: Rc::default(),
        }
    }

    /// Catalogul amprentelor încărcat din baza de date.
    pub fn set_prints(&mut self, prints: Prints) {
        self.prints = Rc::new(prints);
    }

    pub fn prints(&self) -> &Prints {
        &self.prints
    }

    /// Confirmările rămase de dinainte (la pornire).
    pub fn load_pending(&mut self, items: &[Pending]) {
        self.awaiting = items.iter().map(|p| p.path.clone()).collect();
    }

    /// Aprobat în fereastră: pașii de după „Cere confirmare” rulează, dacă fișierul e tot
    /// cel de atunci. Dacă s-a schimbat între timp, regula îl reevaluează de la început.
    pub fn approve(&mut self, fs: &mut dyn Fs, j: &mut dyn Journal, rctx: &RenderCtx<'_>, p: &Pending) -> Outcome {
        self.awaiting.remove(&p.path);
        j.remove_pending(p.id);
        let Some(i) = self.rules.iter().position(|r| r.id == p.rule_id) else { return Outcome::Nothing };
        let Ok((st, created)) = fs.stat_full(&p.path) else { return Outcome::Nothing };
        if st.size != p.size || st.modified != p.mtime {
            self.state.forget(p.rule_id, &p.path);
            return self.process(fs, j, rctx, &p.path, &p.root, None, None);
        }
        let meta = FileMeta::new(&p.path, &p.root, st.size, created, st.modified);
        let rule = &self.rules[i];
        let with_text;
        let rc = if rule.renders_content(rctx.dicts) {
            let text = TextCache::default();
            with_text = RenderCtx { content: FsContent::new(&*fs, &text).load(&meta), ..rctx.clone() };
            &with_text
        } else {
            rctx
        };
        let rep = run_rule_from(fs, j, rule, &meta, rc, p.step);
        let rule_id = rule.id;
        if !rep.gone {
            if let Ok(st) = fs.metadata(&rep.path) {
                self.state.mark_done(rule_id, &rep.path, st);
                if let Some(step) = rep.awaiting {
                    // A doua confirmare în aceeași regulă.
                    self.ask(j, rule_id, &rep.path, &p.root, st, step, rctx.now);
                }
            }
        }
        Outcome::Ran(vec![rep])
    }

    /// Respins: pașii rămași nu rulează; regula nu mai întreabă până nu se schimbă fișierul.
    /// Celelalte reguli au din nou voie la el.
    pub fn reject(&mut self, fs: &mut dyn Fs, j: &mut dyn Journal, rctx: &RenderCtx<'_>, p: &Pending) -> Outcome {
        self.awaiting.remove(&p.path);
        j.remove_pending(p.id);
        self.process(fs, j, rctx, &p.path, &p.root, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn ask(&mut self, j: &mut dyn Journal, rule_id: u64, path: &Path, root: &Path, st: FileStat, step: usize, now: i64) {
        j.add_pending(Pending {
            id: 0,
            rule_id,
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            size: st.size,
            mtime: st.modified,
            step,
            ts: now,
        });
        self.awaiting.insert(path.to_path_buf());
    }

    /// Căile (și tot ce începe cu ele, ex. `organizator.db-wal`) pe care regulile nu le ating.
    pub fn exclude_prefixes(&mut self, paths: &[&Path]) {
        self.exclude = paths.iter().map(|p| p.to_string_lossy().to_lowercase()).collect();
    }

    fn excluded(&self, path: &Path) -> bool {
        if self.exclude.is_empty() {
            return false;
        }
        let p = path.to_string_lossy().to_lowercase();
        self.exclude.iter().any(|x| p.starts_with(x.as_str()))
    }

    /// „Marchează în așteptare”: regula va procesa din nou fișierul (sau, cu `path = None`,
    /// toate fișierele ei), chiar dacă l-a mai procesat sau utilizatorul l-a restaurat.
    pub fn mark_pending(&mut self, j: &mut dyn Journal, rule: u64, path: Option<&Path>) {
        match path {
            Some(p) => self.state.forget(rule, p),
            None => self.state.forget_rule(rule),
        }
        j.forget_restored(rule, path);
        self.failures.retain(|(r, p), _| !(*r == rule && path.is_none_or(|x| x == p)));
    }

    /// Există reguli active al căror rezultat se schimbă odată cu trecerea timpului?
    pub fn needs_periodic_scan(&self) -> bool {
        self.rules.iter().any(|r| r.enabled && r.condition.is_time_relative())
    }

    /// Momentul celei mai apropiate reîncercări după un eșec.
    pub fn next_retry_at(&self) -> Option<i64> {
        self.failures.values().map(|r| r.offer).min()
    }

    /// Căile ale căror reîncercări sunt scadente, de dat lui `process_path`.
    pub fn take_due_retries(&mut self, now: i64) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for ((_, p), r) in self.failures.iter_mut() {
            if r.offer <= now {
                r.offer = now + 3600;
                if !out.contains(p) {
                    out.push(p.clone());
                }
            }
        }
        out
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Confirmările a căror regulă nu mai întreabă (ștearsă, oprită, trecută în simulare sau
    /// fără „Cere confirmare”) dispar. Regula uită și fișierul, ca să-l reevalueze dacă
    /// revine la lucru real. Întoarce câte s-au șters.
    pub fn drop_stale_pending(&mut self, j: &mut dyn Journal) -> usize {
        let mut n = 0;
        for p in j.pending() {
            let asks = self.rules.iter().any(|r| {
                r.id == p.rule_id && r.enabled && !r.simulate && r.actions.iter().any(|a| matches!(a, Action::Confirm))
            });
            if !asks {
                j.remove_pending(p.id);
                self.awaiting.remove(&p.path);
                self.state.forget(p.rule_id, &p.path);
                n += 1;
            }
        }
        n
    }

    /// Înlocuiește regulile (după o editare). Starea fișierelor se păstrează.
    pub fn set_rules(&mut self, rules: Vec<Rule>) {
        self.rules = rules.into_iter().map(Rule::optimized).collect();
        let ids: Vec<u64> = self.rules.iter().map(|r| r.id).collect();
        self.failures.retain(|(r, _), _| ids.contains(r));
    }

    /// Parcurge toate folderele monitorizate și procesează fișierele.
    pub fn scan(&mut self, fs: &mut dyn Fs, j: &mut dyn Journal, rctx: &RenderCtx<'_>) -> ScanReport {
        self.scan_with(fs, j, rctx, &mut |_, _| {})
    }

    /// Ca `scan`, cu `progress(gata, total)` după fiecare fișier. Întâi se listează tot
    /// (rapid), ca totalul să fie cunoscut; metadatele din listare se recitesc oricum
    /// înainte de prima acțiune.
    pub fn scan_with(
        &mut self,
        fs: &mut dyn Fs,
        j: &mut dyn Journal,
        rctx: &RenderCtx<'_>,
        progress: &mut dyn FnMut(usize, usize),
    ) -> ScanReport {
        let mut report = ScanReport::default();
        let mut roots: Vec<(PathBuf, bool)> = Vec::new();
        for r in self.rules.iter().filter(|r| r.enabled) {
            for f in &r.folders {
                let key = (f.path.clone(), f.recursive);
                if !roots.contains(&key) {
                    roots.push(key);
                }
            }
        }
        let mut files: Vec<(PathBuf, usize, Option<(FileStat, i64)>)> = Vec::new();
        for (ri, (root, recursive)) in roots.iter().enumerate() {
            let mut stack = vec![root.clone()];
            while let Some(dir) = stack.pop() {
                // Un folder indisponibil (unitate de rețea deconectată) se așteaptă.
                let Ok(entries) = fs.list_dir(&dir) else { continue };
                for e in entries {
                    if e.is_dir {
                        if *recursive {
                            stack.push(e.path);
                        }
                    } else {
                        files.push((e.path, ri, e.stat));
                    }
                }
            }
        }
        let total = files.len();
        progress(0, total);
        let dirs = DirCache::new(self.prints.clone());
        for (i, (path, ri, stat)) in files.into_iter().enumerate() {
            report.seen += 1;
            match self.process(fs, j, rctx, &path, &roots[ri].0, stat, Some(&dirs)) {
                Outcome::Ran(rs) => report.processed.extend(rs),
                Outcome::Waiting => {
                    report.waiting += 1;
                    let left = self.settle_left(&path, rctx.now);
                    report.waiting_paths.push((path, left));
                }
                Outcome::Busy => {
                    report.waiting += 1;
                    report.waiting_paths.push((path, IN_USE_RECHECK));
                }
                Outcome::Nothing => {}
            }
            progress(i + 1, total);
        }
        report
    }

    /// Procesează un fișier (eveniment de la watcher).
    pub fn process_path(
        &mut self,
        fs: &mut dyn Fs,
        j: &mut dyn Journal,
        rctx: &RenderCtx<'_>,
        path: &Path,
        root: &Path,
    ) -> Outcome {
        self.process(fs, j, rctx, path, root, None, None)
    }

    /// `dirs` = listările păstrate pe trecere (doar la scanare).
    /// `known` = metadatele din listarea folderului (dacă există); se folosesc doar pentru
    /// condiții, iar înainte de prima acțiune se recitesc de pe disc.
    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        fs: &mut dyn Fs,
        j: &mut dyn Journal,
        rctx: &RenderCtx<'_>,
        path: &Path,
        root: &Path,
        known: Option<(FileStat, i64)>,
        dirs: Option<&DirCache>,
    ) -> Outcome {
        if is_temp_download(path) || self.excluded(path) || self.awaiting.contains(path) {
            return Outcome::Nothing;
        }
        // Un singur fișier (eveniment, aprobare): listări proprii, catalogul comun.
        let local;
        let dirs = match dirs {
            Some(d) => Some(d),
            None => {
                local = DirCache::new(self.prints.clone());
                Some(&local)
            }
        };
        let mut fresh = known.is_none();
        let Some((stat, created)) = known.or_else(|| fs.stat_full(path).ok()) else {
            // Fișierul nu mai există: nu mai are ce reîncerca.
            if !self.failures.is_empty() {
                self.failures.retain(|(_, p), _| p != path);
            }
            return Outcome::Nothing;
        };
        let mut cur = FileMeta::new(path, root, stat.size, created, stat.modified);
        let ectx = EvalCtx { now: rctx.now };
        let mut cur_stat = stat;
        let mut ran = Vec::new();
        let mut stable_checked = false;
        let text = TextCache::default();

        for i in 0..self.rules.len() {
            let rule = &self.rules[i];
            if !rule.enabled || rule.watching(&cur.path).is_none() {
                continue;
            }
            // O regulă în simulare ține minte separat ce a simulat: oprită simularea, rulează
            // pe aceleași fișiere de-adevăratelea.
            let state_id = if rule.simulate { sim_key(rule.id) } else { rule.id };
            if self.state.is_done(state_id, &cur.path, cur_stat) {
                continue;
            }
            // Conținutul se citește doar din fișiere care nu se mai scriu.
            if !stable_checked && rule.condition.uses_content() {
                if !self.stability.is_stable(&cur.path, cur_stat, rctx.now, self.settle_secs) {
                    return Outcome::Waiting;
                }
                stable_checked = true;
            }
            if !rule.applies_with(&cur, &ectx, &FsContent::with_dirs(&*fs, &text, dirs)) {
                if !self.failures.is_empty() {
                    self.failures.remove(&(rule.id, cur.path.clone()));
                }
                continue;
            }
            if !fresh {
                // Prima regulă care se potrivește: valori proaspete înainte de orice decizie.
                let Ok((st, created)) = fs.stat_full(&cur.path) else { return Outcome::Nothing };
                fresh = true;
                cur_stat = st;
                cur.size = st.size;
                cur.modified = st.modified;
                cur.created = created;
                if self.state.is_done(state_id, &cur.path, cur_stat)
                    || !rule.applies_with(&cur, &ectx, &FsContent::with_dirs(&*fs, &text, dirs))
                {
                    continue;
                }
            }
            let key = (rule.id, cur.path.clone());
            // După un eșec temporar se așteaptă pauza; regulile următoare nu trec înaintea ei.
            if let Some(r) = self.failures.get(&key) {
                if rctx.now < r.due {
                    break;
                }
            }
            // Utilizatorul a anulat procesarea și a readus fișierul aici: rămâne pe loc.
            if j.restored_to(rule.id, &cur.path, cur_stat.size, cur_stat.modified) {
                self.state.mark_done(rule.id, &cur.path, cur_stat);
                continue;
            }
            if !stable_checked {
                if !self.stability.is_stable(&cur.path, cur_stat, rctx.now, self.settle_secs) {
                    return Outcome::Waiting;
                }
                stable_checked = true;
            }
            // Deschis de alt program (descărcare oprită o vreme, document deschis în Word) sau
            // locțiitor de descărcare: se așteaptă, fără eroare și fără reîncercări numărate.
            if ran.is_empty() && (fs.in_use(&cur.path) || download_in_progress(&*fs, &cur.path)) {
                return Outcome::Busy;
            }
            // Dicționarele pe conținut primesc textul (citit o dată, ca și pentru condiții).
            let with_text;
            let rc = if rule.renders_content(rctx.dicts) {
                with_text = RenderCtx { content: FsContent::new(&*fs, &text).load(&cur), ..rctx.clone() };
                &with_text
            } else {
                rctx
            };
            if rule.simulate {
                // Pe un sistem de fișiere simulat, cu jurnalul marcând totul „simulat”.
                // „Cere confirmare” se consideră aprobat: jurnalul arată tot ce s-ar face
                // (altfel o regulă cu confirmare nu lăsa nicio urmă în simulare).
                let mut dry = DryFs::new(&*fs);
                let mut sj = SimJournal::new(j);
                let mut rep = run_rule(&mut dry, &mut sj, rule, &cur, rc);
                while let Some(step) = rep.awaiting.filter(|_| !rep.gone) {
                    let Ok((st, created)) = dry.stat_full(&rep.path) else { break };
                    let at = FileMeta::new(&rep.path, &cur.root, st.size, created, st.modified);
                    let next = run_rule_from(&mut dry, &mut sj, rule, &at, rc, step);
                    ran.push(rep);
                    rep = next;
                }
                ran.push(rep);
                self.state.mark_done(state_id, &cur.path, cur_stat);
                continue;
            }
            let rep = run_rule(fs, j, rule, &cur, rc);
            let rule_id = rule.id;

            // Eșec înainte ca fișierul să fie mutat (blocat de antivirus, unitate lipsă):
            // se reîncearcă mai târziu, cu pauze tot mai lungi.
            // O comandă eșuată nu se repetă singură: poate să nu fie sigur de rulat de două ori.
            let failed_run = rep
                .steps
                .iter()
                .position(|st| matches!(st, StepStatus::Failed(_)))
                .is_some_and(|i| matches!(rule.actions.get(i), Some(Action::Run { .. } | Action::Command { .. })));
            if !rep.ok() && !rep.gone && rep.path == cur.path && !failed_run {
                let attempts = self.failures.get(&key).map_or(0, |r| r.attempts) + 1;
                if let Some(wait) = RETRY_BACKOFF.get(attempts - 1) {
                    let due = rctx.now + wait;
                    self.failures.insert(key, Retry { attempts, due, offer: due });
                    ran.push(rep);
                    break;
                }
            }
            self.failures.remove(&key);

            // Oprită la „Cere confirmare”: fișierul rămâne al acestei reguli până la răspuns.
            let stop = !rule.continue_after_move && rep.path != cur.path || rep.gone || !rep.ok() || rep.awaiting.is_some();
            // Fișierul la noua lui cale e deja procesat de această regulă: previne bucle
            // (de pildă o redenumire în același folder care s-ar potrivi din nou).
            if !rep.gone {
                if let Ok(st) = fs.metadata(&rep.path) {
                    self.state.mark_done(rule_id, &rep.path, st);
                    cur_stat = st;
                    if let Some(step) = rep.awaiting {
                        let root = cur.root.clone();
                        self.ask(j, rule_id, &rep.path, &root, st, step, rctx.now);
                    }
                }
            }
            if rep.path != cur.path {
                self.stability.forget(&cur.path);
                cur.path = rep.path.clone();
            }
            ran.push(rep);
            if stop {
                break;
            }
        }
        if stable_checked {
            // Procesat: urmărirea stabilității nu mai e necesară (altfel lista crește la nesfârșit).
            self.stability.forget(path);
        }
        if ran.is_empty() {
            Outcome::Nothing
        } else {
            Outcome::Ran(ran)
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    Nothing,
    /// Fișierul încă se modifică (dimensiune sau dată schimbate de curând).
    Waiting,
    /// Fișierul e deschis de alt program sau e locțiitorul unei descărcări.
    Busy,
    Ran(Vec<RunReport>),
}

/// Textul ultimului fișier citit, după cale, dimensiune și dată (toate regulile îl împart).
pub type TextCache = RefCell<Option<((PathBuf, u64, i64), Option<Arc<str>>)>>;

/// Citește textul fișierelor pentru condițiile și dicționarele pe conținut.
pub struct FsContent<'f> {
    fs: &'f dyn Fs,
    cache: &'f TextCache,
    dirs: Option<&'f DirCache>,
    /// Dublurile se confirmă octet cu octet (înaintea acțiunilor). Previzualizarea și
    /// listele se bazează pe amprente.
    verify: bool,
}

impl<'f> FsContent<'f> {
    pub fn new(fs: &'f dyn Fs, cache: &'f TextCache) -> Self {
        FsContent { fs, cache, dirs: None, verify: true }
    }
    /// Cu listările folderelor păstrate pe toată trecerea (scanare, previzualizare).
    pub fn with_dirs(fs: &'f dyn Fs, cache: &'f TextCache, dirs: Option<&'f DirCache>) -> Self {
        FsContent { fs, cache, dirs, verify: true }
    }
    /// Fără confirmarea octet cu octet: pentru afișare, nu pentru acțiuni.
    pub fn by_prints(mut self) -> Self {
        self.verify = false;
        self
    }
}

impl ContentLoader for FsContent<'_> {
    /// Originalul: cel mai vechi fișier cu același conținut, din același folder sau din
    /// `within` (vezi [`DirCache::original`]).
    fn verdict(&self, meta: &FileMeta, key: u64) -> Option<bool> {
        self.dirs.and_then(|d| d.prints().verdict(&meta.path, meta.size, meta.modified, key))
    }

    fn remember(&self, meta: &FileMeta, key: u64, matched: bool) {
        if let Some(d) = self.dirs {
            d.prints().remember(&meta.path, meta.size, meta.modified, key, matched);
        }
    }

    fn duplicate_of(&self, meta: &FileMeta, within: &[WatchedFolder]) -> Option<PathBuf> {
        match self.dirs {
            Some(c) => c.original(self.fs, meta, within, self.verify),
            None => DirCache::default().original(self.fs, meta, within, self.verify),
        }
    }

    fn load(&self, meta: &FileMeta) -> Option<Arc<str>> {
        let key = (meta.path.clone(), meta.size, meta.modified);
        if let Some((k, t)) = &*self.cache.borrow() {
            if *k == key {
                return t.clone();
            }
        }
        let t = load_text(self.fs, meta);
        *self.cache.borrow_mut() = Some((key, t.clone()));
        t
    }
}

/// Textul fișierului, în minuscule; `None` dacă nu are text, e prea mare sau nu se poate citi.
pub fn load_text(fs: &dyn Fs, meta: &FileMeta) -> Option<Arc<str>> {
    let ext = meta.extension();
    let limit = org_content::read_limit(&ext, meta.size)?;
    let data = fs.read_prefix(&meta.path, limit).ok()?;
    let text = org_content::extract(&ext, &data)?;
    Some(Arc::from(text.to_lowercase()))
}

/// Cheia de stare a unei reguli în simulare (separată de cea reală).
pub fn sim_key(rule_id: u64) -> u64 {
    rule_id ^ 0x5349_4d55_4c41_5245 // „SIMULARE”
}

/// Jurnalul unei reguli în simulare: intrările se scriu doar la final, ca „simulat”
/// (nicio intrare „în lucru” pe care recuperarea de la pornire ar încerca s-o refacă).
pub struct SimJournal<'j> {
    inner: &'j mut dyn Journal,
    open: Vec<(u64, org_journal::NewEntry)>,
    next: u64,
}

impl<'j> SimJournal<'j> {
    pub fn new(inner: &'j mut dyn Journal) -> Self {
        SimJournal { inner, open: Vec::new(), next: 0 }
    }
}

impl Journal for SimJournal<'_> {
    fn next_run_id(&mut self) -> u64 {
        self.inner.next_run_id()
    }
    fn begin(&mut self, e: org_journal::NewEntry) -> u64 {
        self.next += 1;
        self.open.push((self.next, e));
        self.next
    }
    fn finish(&mut self, id: u64, status: org_journal::Status) {
        let Some(i) = self.open.iter().position(|(k, _)| *k == id) else { return };
        let (_, e) = self.open.remove(i);
        let real = self.inner.begin(e);
        let st = match status {
            org_journal::Status::Done => org_journal::Status::Simulated,
            other => other,
        };
        self.inner.finish(real, st);
    }
    fn get(&self, _id: u64) -> Option<org_journal::Entry> {
        None
    }
    fn in_progress(&self) -> Vec<org_journal::Entry> {
        Vec::new()
    }
    fn by_run(&self, run_id: u64) -> Vec<org_journal::Entry> {
        self.inner.by_run(run_id)
    }
}
