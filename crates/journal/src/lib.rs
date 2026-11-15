//! Jurnalul tranzacțional: fiecare operație se înregistrează înainte să se execute
//! (starea `InProgress`) și se închide după. La pornire, [`recover`] reconciliază
//! operațiile rămase în curs după o oprire bruscă. [`undo`] anulează o operație.
//!
//! Stocarea e abstractă (`Journal`); aici există varianta în memorie. Adaptorul SQLite
//! implementează același trait.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use org_fs::{move_file, part_path, unique_path, Fs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Move,
    Copy,
    Rename,
    ChangeExt,
    Trash,
    Delete,
    Run,
    /// `src` is the archive, `dst` the folder it was extracted into.
    Extract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    InProgress,
    Done,
    Failed(String),
    Undone,
    /// Regulă în simulare: operația ar fi avut loc, dar nu s-a făcut.
    Simulated,
}

/// Ce se scrie înainte de operație. `size` și `mtime` sunt cele ale sursei în acel
/// moment: servesc la recuperare (a rămas o copie completă?) și la anulare (a fost
/// fișierul modificat de atunci?).
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub run_id: u64,
    pub rule_id: u64,
    pub ts: i64,
    pub kind: OpKind,
    pub src: PathBuf,
    pub dst: Option<PathBuf>,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u64,
    pub run_id: u64,
    pub rule_id: u64,
    pub ts: i64,
    pub kind: OpKind,
    pub src: PathBuf,
    pub dst: Option<PathBuf>,
    pub size: u64,
    pub mtime: i64,
    pub status: Status,
}

pub trait Journal {
    /// Număr nou de rulare; grupează operațiile unei evaluări, pentru anulare pe lot.
    fn next_run_id(&mut self) -> u64;
    /// Scrie intenția, cu starea `InProgress`; întoarce id-ul intrării.
    fn begin(&mut self, e: NewEntry) -> u64;
    fn finish(&mut self, id: u64, status: Status);
    fn get(&self, id: u64) -> Option<Entry>;
    fn in_progress(&self) -> Vec<Entry>;
    fn by_run(&self, run_id: u64) -> Vec<Entry>;
    /// Închide intrarea ca anulată și ține minte unde a revenit fișierul.
    fn mark_undone(&mut self, id: u64, restored: &Path) {
        let _ = restored;
        self.finish(id, Status::Undone);
    }
    /// Utilizatorul a restaurat aici, prin anulare, un fișier pe care regula îl procesase
    /// (aceeași dimensiune și dată)? Atunci regula nu trebuie să-l proceseze din nou.
    fn restored_to(&self, rule_id: u64, path: &Path, size: u64, mtime: i64) -> bool {
        let _ = (rule_id, path, size, mtime);
        false
    }
    /// „În așteptare” bate restaurarea: regula are voie din nou la fișier.
    /// `path = None` = toate fișierele regulii.
    fn forget_restored(&mut self, rule_id: u64, path: Option<&Path>) {
        let _ = (rule_id, path);
    }
    /// O regulă s-a oprit la „Cere confirmare”; un fișier și o regulă au cel mult o cerere.
    fn add_pending(&mut self, p: Pending) {
        let _ = p;
    }
    /// Cererile care așteaptă confirmarea, cele mai noi primele.
    fn pending(&self) -> Vec<Pending> {
        Vec::new()
    }
    fn remove_pending(&mut self, id: u64) {
        let _ = id;
    }
}

/// O confirmare cerută: regula, fișierul (cu dimensiunea și data de atunci) și pasul de
/// la care se continuă după aprobare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub rule_id: u64,
    pub path: PathBuf,
    /// Folderul monitorizat prin care a venit fișierul (pentru căile relative).
    pub root: PathBuf,
    pub size: u64,
    pub mtime: i64,
    pub step: usize,
    pub ts: i64,
}

#[derive(Debug, Default)]
pub struct MemJournal {
    entries: Vec<Entry>,
    last_run: u64,
    /// (regulă, unde a revenit fișierul, dimensiune, dată) pentru fiecare anulare.
    restored: Vec<(u64, PathBuf, u64, i64)>,
    pending: Vec<Pending>,
    last_pending: u64,
}

impl MemJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Doar pentru teste: simulează o oprire bruscă. Intrările închise cu eroarea
    /// injectată nu s-ar fi apucat să fie închise, deci redevin `InProgress`.
    pub fn simulate_crash(&mut self) {
        for e in &mut self.entries {
            if matches!(&e.status, Status::Failed(m) if m.contains("injected")) {
                e.status = Status::InProgress;
            }
        }
    }
}

impl Journal for MemJournal {
    fn next_run_id(&mut self) -> u64 {
        self.last_run += 1;
        self.last_run
    }

    fn begin(&mut self, e: NewEntry) -> u64 {
        let id = self.entries.len() as u64 + 1;
        self.entries.push(Entry {
            id,
            run_id: e.run_id,
            rule_id: e.rule_id,
            ts: e.ts,
            kind: e.kind,
            src: e.src,
            dst: e.dst,
            size: e.size,
            mtime: e.mtime,
            status: Status::InProgress,
        });
        id
    }

    fn finish(&mut self, id: u64, status: Status) {
        if let Some(e) = self.entries.get_mut(id as usize - 1) {
            e.status = status;
        }
    }

    fn get(&self, id: u64) -> Option<Entry> {
        self.entries.get((id as usize).checked_sub(1)?).cloned()
    }

    fn in_progress(&self) -> Vec<Entry> {
        self.entries.iter().filter(|e| e.status == Status::InProgress).cloned().collect()
    }

    fn by_run(&self, run_id: u64) -> Vec<Entry> {
        self.entries.iter().filter(|e| e.run_id == run_id).cloned().collect()
    }

    fn mark_undone(&mut self, id: u64, restored: &Path) {
        if let Some(e) = self.entries.get_mut(id as usize - 1) {
            e.status = Status::Undone;
            self.restored.push((e.rule_id, restored.to_path_buf(), e.size, e.mtime));
        }
    }

    fn restored_to(&self, rule_id: u64, path: &Path, size: u64, mtime: i64) -> bool {
        self.restored.iter().any(|(r, p, sz, mt)| *r == rule_id && p == path && *sz == size && *mt == mtime)
    }

    fn forget_restored(&mut self, rule_id: u64, path: Option<&Path>) {
        self.restored.retain(|(r, p, _, _)| !(*r == rule_id && path.is_none_or(|x| x == p)));
    }

    fn add_pending(&mut self, mut p: Pending) {
        self.pending.retain(|x| !(x.rule_id == p.rule_id && x.path == p.path));
        self.last_pending += 1;
        p.id = self.last_pending;
        self.pending.push(p);
    }

    fn pending(&self) -> Vec<Pending> {
        self.pending.iter().rev().cloned().collect()
    }

    fn remove_pending(&mut self, id: u64) {
        self.pending.retain(|x| x.id != id);
    }
}

/// Intrările care se pot arăta: toate dacă aplicația e deblocată, altfel doar cele ale
/// regulilor care nu sunt ascunse.
pub fn visible_entries<'a>(
    entries: &'a [Entry],
    hidden_rules: &'a HashSet<u64>,
    unlocked: bool,
) -> impl Iterator<Item = &'a Entry> {
    entries.iter().filter(move |e| unlocked || !hidden_rules.contains(&e.rule_id))
}

// ---------- recuperare după oprire bruscă ----------

/// Reconciliază operațiile rămase `InProgress`, comparând ce scrie jurnalul cu ce există
/// pe disc. Nu pierde niciodată un fișier: o copie incompletă se șterge doar când sursa
/// încă există. Întoarce starea nouă a fiecărei intrări reconciliate.
pub fn recover(fs: &mut dyn Fs, j: &mut dyn Journal) -> Vec<(u64, Status)> {
    let mut out = Vec::new();
    for e in j.in_progress() {
        let src_exists = fs.exists(&e.src);
        let dst = e.dst.clone();
        let dst_exists = dst.as_ref().is_some_and(|d| fs.exists(d));
        let status = match e.kind {
            OpKind::Move | OpKind::Rename | OpKind::ChangeExt => match (src_exists, dst_exists) {
                (true, false) => Status::Failed("întreruptă înainte de început".into()),
                (false, true) => Status::Done,
                (true, true) if e.kind == OpKind::Move => {
                    // Copiere între volume întreruptă: copia completă înseamnă că a rămas
                    // doar ștergerea sursei; una incompletă se aruncă.
                    let d = dst.as_ref().unwrap();
                    if fs.metadata(d).is_ok_and(|s| s.size == e.size) {
                        match fs.remove_file(&e.src) {
                            Ok(()) => Status::Done,
                            Err(err) => Status::Failed(format!("sursa nu s-a putut șterge: {err}")),
                        }
                    } else {
                        let _ = fs.remove_file(d);
                        Status::Failed("copie incompletă ștearsă, sursa e intactă".into())
                    }
                }
                (true, true) => Status::Failed("sursa și destinația există".into()),
                (false, false) => Status::Failed("fișierul lipsește".into()),
            },
            OpKind::Copy => match (&dst, dst_exists) {
                (Some(d), true) => {
                    if fs.metadata(d).is_ok_and(|s| s.size == e.size) {
                        Status::Done
                    } else {
                        let _ = fs.remove_file(d);
                        Status::Failed("copie incompletă ștearsă".into())
                    }
                }
                _ => Status::Failed("întreruptă înainte de început".into()),
            },
            OpKind::Trash | OpKind::Delete => {
                if src_exists {
                    Status::Failed("întreruptă înainte de început".into())
                } else {
                    Status::Done
                }
            }
            OpKind::Run => Status::Failed("rezultat necunoscut după întrerupere".into()),
            // The folder gets its final name in one rename at the very end: if it is there, the
            // extraction finished. Otherwise the half-extracted folder is thrown away.
            OpKind::Extract => match &dst {
                Some(_) if dst_exists => Status::Done,
                Some(d) => {
                    let part = part_path(d);
                    if fs.exists(&part) {
                        let _ = fs.remove_tree(&part);
                    }
                    Status::Failed("extraction interrupted; the partial folder was removed".into())
                }
                None => Status::Failed("întreruptă înainte de început".into()),
            },
        };
        j.finish(e.id, status.clone());
        out.push((e.id, status));
    }
    out
}

// ---------- restaurare ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// Fișierul a revenit aici (poate cu sufix, dacă locul vechi era ocupat).
    Restored(PathBuf),
    /// Fișierul a fost modificat după operație; se cere confirmare (sau `force`).
    NeedsConfirmation(&'static str),
    Unrecoverable(&'static str),
    NotUndoable,
}

/// Anulează o operație încheiată cu succes. Nu suprascrie niciodată: dacă locul vechi
/// e ocupat, fișierul revine cu un sufix (`_2`). Un fișier modificat după operație cere
/// confirmare, în afară de cazul în care `force` e adevărat.
pub fn undo(fs: &mut dyn Fs, j: &mut dyn Journal, id: u64, force: bool) -> io::Result<UndoOutcome> {
    let Some(e) = j.get(id) else {
        return Ok(UndoOutcome::Unrecoverable("intrarea nu există"));
    };
    match e.status {
        Status::Done => {}
        Status::Undone => return Ok(UndoOutcome::Unrecoverable("deja anulată")),
        Status::Simulated => return Ok(UndoOutcome::Unrecoverable("simulare: nu s-a schimbat nimic")),
        _ => return Ok(UndoOutcome::Unrecoverable("operația nu s-a încheiat cu succes")),
    }
    let unchanged = |fs: &dyn Fs, p: &PathBuf| {
        fs.metadata(p).map(|s| s.size == e.size && s.modified == e.mtime).unwrap_or(false)
    };

    let outcome = match e.kind {
        OpKind::Move | OpKind::Rename | OpKind::ChangeExt => {
            let dst = e.dst.clone().expect("mutarea are destinație");
            if !fs.exists(&dst) {
                return Ok(UndoOutcome::Unrecoverable("fișierul nu mai e la destinație"));
            }
            if !force && !unchanged(fs, &dst) {
                return Ok(UndoOutcome::NeedsConfirmation("fișierul a fost modificat după mutare"));
            }
            if let Some(parent) = e.src.parent() {
                fs.create_dir_all(parent)?;
            }
            let target = unique_path(fs, &e.src);
            move_file(fs, &dst, &target)?;
            UndoOutcome::Restored(target)
        }
        OpKind::Copy => {
            let dst = e.dst.clone().expect("copierea are destinație");
            if !fs.exists(&dst) {
                return Ok(UndoOutcome::Unrecoverable("copia nu mai există"));
            }
            if !force && !unchanged(fs, &dst) {
                return Ok(UndoOutcome::NeedsConfirmation("copia a fost modificată"));
            }
            // Copia merge în Coș (recuperabilă); fără Coș disponibil, se șterge.
            match fs.trash(&dst) {
                Err(err) if err.kind() == io::ErrorKind::Unsupported => fs.remove_file(&dst)?,
                other => other?,
            }
            UndoOutcome::Restored(e.src.clone())
        }
        OpKind::Trash => {
            if fs.exists(&e.src) {
                // Coșul restaurează doar la calea originală; confirmarea nu ar schimba nimic.
                return Ok(UndoOutcome::Unrecoverable("locul vechi e ocupat de alt fișier; mută-l întâi"));
            }
            match fs.restore_from_trash(&e.src) {
                Ok(()) => UndoOutcome::Restored(e.src.clone()),
                Err(_) => return Ok(UndoOutcome::Unrecoverable("fișierul nu mai e în Coș")),
            }
        }
        OpKind::Extract => {
            let dir = e.dst.clone().expect("extraction has a folder");
            if !fs.exists(&dir) {
                return Ok(UndoOutcome::Unrecoverable("the extracted folder no longer exists"));
            }
            // Extracted files keep the dates from the archive: anything newer than the
            // operation was changed or added since.
            if !force && changed_since(fs, &dir, e.ts) {
                return Ok(UndoOutcome::NeedsConfirmation("files in the extracted folder changed since"));
            }
            match fs.trash(&dir) {
                Err(err) if err.kind() == io::ErrorKind::Unsupported => fs.remove_tree(&dir)?,
                other => other?,
            }
            // The archive itself never moved: it is what "comes back", so the rule does not
            // extract it again on its own.
            UndoOutcome::Restored(e.src.clone())
        }
        OpKind::Delete => return Ok(UndoOutcome::Unrecoverable("șters definitiv")),
        OpKind::Run => return Ok(UndoOutcome::NotUndoable),
    };
    if let UndoOutcome::Restored(p) = &outcome {
        j.mark_undone(id, p);
    }
    Ok(outcome)
}

/// Does the folder hold a file modified after `ts` (or one that cannot be read)?
fn changed_since(fs: &dyn Fs, dir: &Path, ts: i64) -> bool {
    let Ok(entries) = fs.list_dir(dir) else { return true };
    entries.iter().any(|e| {
        if e.is_dir {
            changed_since(fs, &e.path, ts)
        } else {
            fs.metadata(&e.path).map_or(true, |m| m.modified > ts)
        }
    })
}

/// Anulează toate operațiile unei rulări, în ordine inversă.
pub fn undo_run(
    fs: &mut dyn Fs,
    j: &mut dyn Journal,
    run_id: u64,
    force: bool,
) -> Vec<(u64, io::Result<UndoOutcome>)> {
    let mut entries = j.by_run(run_id);
    entries.reverse();
    entries
        .into_iter()
        .filter(|e| e.status == Status::Done)
        .map(|e| (e.id, undo(fs, j, e.id, force)))
        .collect()
}
