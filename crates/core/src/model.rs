use std::path::{Path, PathBuf};

use std::collections::HashMap;

use crate::condition::{Condition, ContentLoader, EvalCtx, Prepared, RuleError, TextField};
use crate::meta::FileMeta;
use crate::template::{Dictionary, Template};

/// Ce se face când în destinație există deja un fișier cu același nume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// `invoice.docx` devine `invoice_2.docx`, apoi `_3` etc. Implicit și cel mai sigur.
    Rename,
    /// Fișierul nou nu se atinge.
    Skip,
    /// Fișierul existent se înlocuiește. Nu se poate anula complet.
    Overwrite,
    /// Dacă în folderul destinație există deja un fișier cu același conținut (oricum s-ar
    /// numi), cel nou e o dublură: la mutare și redenumire merge în Coș, la copiere nu se
    /// mai copiază. Altfel ca `Rename`.
    Dedupe,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Move { dest: Template, conflict: Conflict, keep_structure: bool, remove_empty: bool },
    /// Cu `then_target`, acțiunile următoare lucrează pe copie, nu pe sursă.
    Copy { dest: Template, conflict: Conflict, keep_structure: bool, then_target: bool },
    /// Păstrează extensia; se schimbă doar numele.
    Rename { name: Template, conflict: Conflict },
    ChangeExtension { ext: Template },
    /// Mută în Coșul de reciclare.
    Trash { remove_empty: bool },
    /// Ștergere definitivă; nu se poate anula.
    Delete { remove_empty: bool },
    /// Rulează un program direct, fără shell; argumentele nu pot injecta comenzi.
    Run { program: Template, args: Vec<Template> },
    /// O linie de comandă prin `cmd /C` (comenzi interne, redirecționări, `&&`). Valorile
    /// variabilelor se pun între ghilimele, ca un nume de fișier să nu poată adăuga comenzi.
    Command { line: Template },
    /// Acțiunile următoare așteaptă confirmarea din fereastră (Aprobă / Respinge).
    Confirm,
    /// Extracts the archive into a new folder (`dest`, usually `{folder}\{name}`). The archive
    /// stays where it is, so the next actions still work on it. If the folder already exists,
    /// `conflict` is `Skip` (do nothing) or anything else (a new folder `name_2`).
    Extract { dest: Template, conflict: Conflict },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFolder {
    pub path: PathBuf,
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// O regulă ascunsă nu apare în interfață, în jurnal și în statistici cât timp
    /// aplicația nu e deblocată cu parola. Procesul de fundal o rulează oricum.
    pub hidden: bool,
    pub folders: Vec<WatchedFolder>,
    pub condition: Condition,
    pub actions: Vec<Action>,
    /// Dacă e fals, după ce regula a mutat sau a șters fișierul, regulile următoare
    /// nu mai lucrează pe el.
    pub continue_after_move: bool,
    /// Doar simulare: acțiunile se scriu în jurnal ca „simulat”, fișierele rămân neatinse,
    /// iar regulile următoare le procesează normal.
    pub simulate: bool,
}

impl Rule {
    pub fn validate(&self) -> Result<(), RuleError> {
        if self.folders.is_empty() {
            return Err(RuleError::NoFolders);
        }
        if self.actions.is_empty() {
            return Err(RuleError::NoActions);
        }
        self.condition.validate()?;
        // După Trash sau Delete nu mai există fișier pe care să lucreze o acțiune.
        let removal = self
            .actions
            .iter()
            .position(|a| matches!(a, Action::Trash { .. } | Action::Delete { .. }));
        if let Some(i) = removal {
            if i + 1 < self.actions.len() {
                return Err(RuleError::ActionAfterRemoval);
            }
        }
        Ok(())
    }

    /// Folderul monitorizat prin care ajunge fișierul la regulă, dacă îl urmărește.
    pub fn watching(&self, file: &Path) -> Option<&WatchedFolder> {
        self.folders.iter().find(|f| {
            file.starts_with(&f.path) && (f.recursive || file.parent() == Some(f.path.as_path()))
        })
    }

    pub fn applies_to(&self, meta: &FileMeta, ctx: &EvalCtx) -> bool {
        self.enabled && self.condition.matches(&Prepared::new(meta), ctx)
    }

    /// Ca `applies_to`, cu cine citește conținutul fișierului.
    pub fn applies_with(&self, meta: &FileMeta, ctx: &EvalCtx, loader: &dyn ContentLoader) -> bool {
        self.enabled && self.condition.matches(&Prepared::with_content(meta, loader), ctx)
    }

    /// Acțiunile folosesc un dicționar care caută în conținutul fișierului?
    pub fn renders_content(&self, dicts: &HashMap<String, Dictionary>) -> bool {
        let uses = |t: &Template| t.named().any(|n| dicts.get(n).is_some_and(|d| d.field == TextField::Content));
        self.actions.iter().any(|a| match a {
            Action::Move { dest, .. } | Action::Copy { dest, .. } | Action::Extract { dest, .. } => uses(dest),
            Action::Rename { name, .. } => uses(name),
            Action::ChangeExtension { ext } => uses(ext),
            Action::Run { program, args } => uses(program) || args.iter().any(uses),
            Action::Command { line } => uses(line),
            Action::Trash { .. } | Action::Delete { .. } | Action::Confirm => false,
        })
    }

    /// Reordonează condițiile după cost; rezultatul logic rămâne același.
    pub fn optimized(mut self) -> Rule {
        self.condition = self.condition.optimized();
        self
    }
}

/// Regulile care se pot arăta: toate dacă aplicația e deblocată, altfel doar cele neascunse.
pub fn visible_rules(rules: &[Rule], unlocked: bool) -> impl Iterator<Item = &Rule> {
    rules.iter().filter(move |r| unlocked || !r.hidden)
}
