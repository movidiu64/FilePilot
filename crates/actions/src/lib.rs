//! Executorul de acțiuni: aplică acțiunile unei reguli pe un fișier, pas cu pas.
//!
//! Fiecare pas care modifică ceva se scrie în jurnal înainte (`begin`) și se închide
//! după (`finish`). Primul pas eșuat oprește regula: fișierul rămâne unde a ajuns.

use std::io;
use std::path::{Component, Path, PathBuf};

use org_core::{safe_name, Action, Conflict, FileMeta, Mode, RenderCtx, Rule, Template};
use org_fs::{move_file, remove_empty_parents, unique_path, Fs};
use org_journal::{Journal, MemJournal, NewEntry, OpKind, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Done,
    /// Pasul nu s-a executat, dar regula continuă (conflict `Skip`, nimic de făcut).
    Skipped(&'static str),
    Failed(String),
    /// Fișierul era o dublură a celui de la destinație și a mers în Coș (conflict `Dedupe`).
    Deduplicated,
    /// Regula s-a oprit la „Cere confirmare”; restul pașilor așteaptă aprobarea.
    Awaiting,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub run_id: u64,
    pub steps: Vec<StepStatus>,
    /// Unde se află fișierul la sfârșit (urmărit pas cu pas).
    pub path: PathBuf,
    /// Fișierul a fost șters sau trimis în Coș.
    pub gone: bool,
    /// Oprită la „Cere confirmare”: indicele pasului de la care se continuă după aprobare.
    pub awaiting: Option<usize>,
}

impl RunReport {
    pub fn ok(&self) -> bool {
        !self.steps.iter().any(|s| matches!(s, StepStatus::Failed(_)))
    }
}

/// Un nume de fișier valid: o singură componentă, fără separatori, nici `.`/`..`.
fn valid_component(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." || s.contains(['/', '\\', ':']) {
        return false;
    }
    let mut c = Path::new(s).components();
    matches!((c.next(), c.next()), (Some(Component::Normal(_)), None))
}

fn render(t: &Template, meta: &FileMeta, ctx: &RenderCtx<'_>, mode: Mode) -> Result<String, String> {
    t.render(meta, ctx, mode).map_err(|e| format!("șablon: {e:?}"))
}

/// Folderul destinație. O cale relativă („PDF”, „Arhiva\2026”) se socotește față de
/// folderul monitorizat, nu față de folderul curent al procesului (care la pornirea
/// automată e de obicei `C:\Windows\System32`).
/// Componentele numite trec prin [`safe_name`]: Windows ar tăia în tăcere punctele și
/// spațiile de la final, iar jurnalul ar ține minte o cale care nu există.
fn dest_dir(dest: &Template, cur: &FileMeta, rctx: &RenderCtx<'_>) -> Result<PathBuf, String> {
    let raw = PathBuf::from(render(dest, cur, rctx, Mode::Dir)?);
    if raw.as_os_str().is_empty() {
        return Err("destinație goală".into());
    }
    let dir: PathBuf = raw
        .components()
        .map(|c| match c {
            Component::Normal(n) => PathBuf::from(safe_name(&n.to_string_lossy())),
            other => PathBuf::from(other.as_os_str()),
        })
        .collect();
    Ok(if dir.is_relative() { cur.root.join(dir) } else { dir })
}

struct Exec<'a> {
    fs: &'a mut dyn Fs,
    j: &'a mut dyn Journal,
    run_id: u64,
    rule_id: u64,
    now: i64,
}

impl Exec<'_> {
    /// Înregistrează, execută, închide. Întoarce rezultatul operației.
    fn journaled(
        &mut self,
        kind: OpKind,
        src: &Path,
        dst: Option<&Path>,
        op: impl FnOnce(&mut dyn Fs) -> io::Result<()>,
    ) -> Result<(), String> {
        let st = self.fs.metadata(src).ok();
        let id = self.j.begin(NewEntry {
            run_id: self.run_id,
            rule_id: self.rule_id,
            ts: self.now,
            kind,
            src: src.to_path_buf(),
            dst: dst.map(Path::to_path_buf),
            size: st.map_or(0, |s| s.size),
            mtime: st.map_or(0, |s| s.modified),
        });
        match op(self.fs) {
            Ok(()) => {
                self.j.finish(id, Status::Done);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.j.finish(id, Status::Failed(msg.clone()));
                Err(msg)
            }
        }
    }

    /// Un fișier din folderul destinație cu exact același conținut ca `src`, oricum s-ar numi
    /// (`raport (1).pdf` ajunge lângă `raport.pdf`). Se compară doar cele de aceeași dimensiune.
    fn identical_in(&self, src: &Path, dir: &Path) -> Result<Option<PathBuf>, String> {
        let Ok(size) = self.fs.metadata(src).map(|m| m.size) else { return Ok(None) };
        let Ok(entries) = self.fs.list_dir(dir) else { return Ok(None) };
        let mut same: Vec<PathBuf> = entries
            .into_iter()
            .filter(|e| !e.is_dir && e.path != src)
            .filter(|e| match e.stat {
                Some((st, _)) => st.size == size,
                None => self.fs.metadata(&e.path).is_ok_and(|m| m.size == size),
            })
            .map(|e| e.path)
            .collect();
        same.sort();
        for p in same {
            if self.fs.same_content(src, &p).map_err(|e| format!("comparare: {e}"))? {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// Alege ținta după politica de conflict.
    fn resolve(&mut self, src: &Path, target: PathBuf, conflict: Conflict) -> Result<Target, String> {
        if conflict == Conflict::Dedupe {
            if let Some(dir) = target.parent() {
                if let Some(orig) = self.identical_in(src, dir)? {
                    return Ok(Target::Duplicate(orig));
                }
            }
        }
        if !self.fs.exists(&target) {
            return Ok(Target::Path(target));
        }
        match conflict {
            Conflict::Rename => Ok(Target::Path(unique_path(self.fs, &target))),
            Conflict::Skip => Ok(Target::Skip),
            // Identicul ar fi fost găsit mai sus: aici e doar același nume, alt conținut.
            Conflict::Dedupe => Ok(Target::Path(unique_path(self.fs, &target))),
            Conflict::Overwrite => {
                // Fișierul înlocuit merge în Coș, cu intrare proprie în jurnal: anularea
                // rulării îl readuce (în ordine inversă: întâi pleacă fișierul nou, apoi revine cel vechi).
                let t = target.clone();
                self.journaled(OpKind::Trash, &target, None, move |fs| fs.trash(&t))
                    .map_err(|e| format!("suprascriere: {e}"))?;
                Ok(Target::Path(target))
            }
        }
    }

    /// Dublura merge în Coș (se poate restaura din jurnal); intrarea arată și originalul.
    fn trash_duplicate(&mut self, src: &Path, original: &Path) -> Result<StepStatus, String> {
        self.journaled(OpKind::Trash, src, Some(original), |fs| fs.trash(src))?;
        Ok(StepStatus::Deduplicated)
    }
}

enum Target {
    Path(PathBuf),
    /// Conflict „Omite”.
    Skip,
    /// La destinație există deja exact același conținut.
    Duplicate(PathBuf),
}

/// Aplică acțiunile regulii pe fișier. `meta.path` trebuie să existe.
/// Ce ar face o regulă cu un fișier: operațiile, unde ajunge, ce eroare ar apărea.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    /// Operațiile reușite, în ordine: (tip, destinație).
    pub ops: Vec<(OpKind, Option<PathBuf>)>,
    /// Unde ar ajunge fișierul (dacă nu dispare).
    pub path: PathBuf,
    pub gone: bool,
    /// Un pas omis (ex. „Omite” când ținta există).
    pub skipped: bool,
    pub error: Option<String>,
    /// Se oprește la „Cere confirmare”.
    pub awaiting: bool,
    /// A ajuns în Coș ca dublură.
    pub duplicate: bool,
}

/// Rulează regula „pe hârtie”: `fs` trebuie să fie un [`org_fs::DryFs`] (sau un sistem de
/// fișiere de test). Jurnalul e unul temporar, în memorie. Logica e exact cea reală.
pub fn plan_rule(fs: &mut dyn Fs, rule: &Rule, meta: &FileMeta, rctx: &RenderCtx<'_>) -> Planned {
    let mut j = MemJournal::new();
    let rep = run_rule(fs, &mut j, rule, meta, rctx);
    Planned {
        ops: j.entries().iter().filter(|e| e.status == Status::Done).map(|e| (e.kind, e.dst.clone())).collect(),
        path: rep.path,
        gone: rep.gone,
        skipped: rep.steps.iter().any(|st| matches!(st, StepStatus::Skipped(_))),
        error: rep.steps.iter().find_map(|st| match st {
            StepStatus::Failed(m) => Some(m.clone()),
            _ => None,
        }),
        awaiting: rep.awaiting.is_some(),
        duplicate: rep.steps.contains(&StepStatus::Deduplicated),
    }
}

pub fn run_rule(
    fs: &mut dyn Fs,
    journal: &mut dyn Journal,
    rule: &Rule,
    meta: &FileMeta,
    rctx: &RenderCtx<'_>,
) -> RunReport {
    run_rule_from(fs, journal, rule, meta, rctx, 0)
}

/// Ca `run_rule`, începând cu pasul `start` (după o confirmare aprobată).
pub fn run_rule_from(
    fs: &mut dyn Fs,
    journal: &mut dyn Journal,
    rule: &Rule,
    meta: &FileMeta,
    rctx: &RenderCtx<'_>,
    start: usize,
) -> RunReport {
    let run_id = journal.next_run_id();
    let rel = meta.relative_parent();
    let mut cur = meta.clone();
    let mut report = RunReport { run_id, steps: Vec::new(), path: cur.path.clone(), gone: false, awaiting: None };
    let mut ex = Exec { fs, j: journal, run_id, rule_id: rule.id, now: rctx.now };

    for (i, action) in rule.actions.iter().enumerate().skip(start) {
        if matches!(action, Action::Confirm) {
            report.steps.push(StepStatus::Awaiting);
            report.awaiting = Some(i + 1);
            break;
        }
        let r = step(&mut ex, action, &mut cur, rel.as_deref(), rctx);
        let failed = matches!(r, StepStatus::Failed(_));
        let dedup = r == StepStatus::Deduplicated;
        if (matches!(action, Action::Trash { .. } | Action::Delete { .. }) && !failed) || dedup {
            report.gone = true;
        }
        report.steps.push(r);
        report.path = cur.path.clone();
        if failed || dedup {
            break;
        }
    }
    report
}

fn step(
    ex: &mut Exec<'_>,
    action: &Action,
    cur: &mut FileMeta,
    rel: Option<&Path>,
    rctx: &RenderCtx<'_>,
) -> StepStatus {
    match step_inner(ex, action, cur, rel, rctx) {
        Ok(s) => s,
        Err(e) => StepStatus::Failed(e),
    }
}

fn step_inner(
    ex: &mut Exec<'_>,
    action: &Action,
    cur: &mut FileMeta,
    rel: Option<&Path>,
    rctx: &RenderCtx<'_>,
) -> Result<StepStatus, String> {
    let src = cur.path.clone();
    let file_name = cur.file_name();
    match action {
        Action::Move { dest, conflict, keep_structure, remove_empty } => {
            let mut dir = dest_dir(dest, cur, rctx)?;
            if *keep_structure {
                if let Some(r) = rel {
                    dir.push(r);
                }
            }
            let target = match ex.resolve(&src, dir.join(&file_name), *conflict)? {
                Target::Path(t) => t,
                Target::Skip => return Ok(StepStatus::Skipped("există deja la destinație")),
                Target::Duplicate(orig) => return ex.trash_duplicate(&src, &orig),
            };
            if target == src {
                return Ok(StepStatus::Skipped("deja la locul lui"));
            }
            ex.journaled(OpKind::Move, &src, Some(&target), |fs| {
                if let Some(p) = target.parent() {
                    fs.create_dir_all(p)?;
                }
                move_file(fs, &src, &target)
            })?;
            if *remove_empty {
                if let Some(p) = src.parent() {
                    remove_empty_parents(ex.fs, p, &cur.root);
                }
            }
            cur.path = target;
            Ok(StepStatus::Done)
        }
        Action::Copy { dest, conflict, keep_structure, then_target } => {
            let mut dir = dest_dir(dest, cur, rctx)?;
            if *keep_structure {
                if let Some(r) = rel {
                    dir.push(r);
                }
            }
            let target = match ex.resolve(&src, dir.join(&file_name), *conflict)? {
                Target::Path(t) => t,
                Target::Skip => return Ok(StepStatus::Skipped("există deja la destinație")),
                // La copiere sursa rămâne: doar nu se mai face o copie identică.
                Target::Duplicate(_) => return Ok(StepStatus::Skipped("identic la destinație")),
            };
            ex.journaled(OpKind::Copy, &src, Some(&target), |fs| {
                if let Some(p) = target.parent() {
                    fs.create_dir_all(p)?;
                }
                fs.copy(&src, &target).map(|_| ())
            })?;
            if *then_target {
                cur.path = target;
            }
            Ok(StepStatus::Done)
        }
        Action::Rename { name, conflict } => {
            let stem = render(name, cur, rctx, Mode::Path)?;
            if !valid_component(&stem) {
                return Err(format!("nume invalid: {stem:?}"));
            }
            let ext = cur.extension();
            let new_name = safe_name(&if ext.is_empty() { stem } else { format!("{stem}.{ext}") });
            let target = src.with_file_name(new_name);
            if target == src {
                return Ok(StepStatus::Skipped("același nume"));
            }
            let target = match ex.resolve(&src, target, *conflict)? {
                Target::Path(t) => t,
                Target::Skip => return Ok(StepStatus::Skipped("există deja un fișier cu acest nume")),
                Target::Duplicate(orig) => return ex.trash_duplicate(&src, &orig),
            };
            ex.journaled(OpKind::Rename, &src, Some(&target), |fs| move_file(fs, &src, &target))?;
            cur.path = target;
            Ok(StepStatus::Done)
        }
        Action::ChangeExtension { ext } => {
            let e = render(ext, cur, rctx, Mode::Path)?;
            let e = e.trim_start_matches('.');
            if !valid_component(e) {
                return Err(format!("extensie invalidă: {e:?}"));
            }
            let target = src.with_file_name(safe_name(&format!("{}.{e}", cur.stem())));
            if target == src {
                return Ok(StepStatus::Skipped("aceeași extensie"));
            }
            let target = unique_path(ex.fs, &target);
            ex.journaled(OpKind::ChangeExt, &src, Some(&target), |fs| move_file(fs, &src, &target))?;
            cur.path = target;
            Ok(StepStatus::Done)
        }
        Action::Trash { remove_empty } => {
            ex.journaled(OpKind::Trash, &src, None, |fs| fs.trash(&src))?;
            if *remove_empty {
                if let Some(p) = src.parent() {
                    remove_empty_parents(ex.fs, p, &cur.root);
                }
            }
            Ok(StepStatus::Done)
        }
        Action::Delete { remove_empty } => {
            ex.journaled(OpKind::Delete, &src, None, |fs| fs.remove_file(&src))?;
            if *remove_empty {
                if let Some(p) = src.parent() {
                    remove_empty_parents(ex.fs, p, &cur.root);
                }
            }
            Ok(StepStatus::Done)
        }
        Action::Command { line } => {
            let line = render(line, cur, rctx, Mode::Shell)?;
            if line.trim().is_empty() {
                return Err("comandă lipsă".into());
            }
            let mut code = 0;
            ex.journaled(OpKind::Run, &src, None, |fs| {
                code = fs.run_shell(&line)?;
                if code != 0 {
                    return Err(io::Error::other(format!("cod de ieșire {code}")));
                }
                Ok(())
            })?;
            Ok(StepStatus::Done)
        }
        // Tratată în `run_rule_from`; aici doar ca potrivirea să fie completă.
        Action::Confirm => Ok(StepStatus::Awaiting),
        Action::Run { program, args } => {
            let prog = render(program, cur, rctx, Mode::Raw)?;
            if prog.is_empty() {
                return Err("program lipsă".into());
            }
            let mut argv = Vec::with_capacity(args.len());
            for a in args {
                argv.push(render(a, cur, rctx, Mode::Raw)?);
            }
            let mut code = 0;
            ex.journaled(OpKind::Run, &src, None, |fs| {
                code = fs.run(&prog, &argv)?;
                if code != 0 {
                    return Err(io::Error::other(format!("cod de ieșire {code}")));
                }
                Ok(())
            })?;
            Ok(StepStatus::Done)
        }
    }
}
