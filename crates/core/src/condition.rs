use std::borrow::Cow;
use std::cell::OnceCell;
use std::sync::Arc;

use crate::meta::FileMeta;

/// Adâncimea maximă a grupurilor imbricate, ca interfața să rămână lizibilă.
pub const MAX_DEPTH: usize = 8;
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Name,
    Extension,
    Path,
    Folder,
    /// Textul din fișier (text simplu, DOCX, XLSX, ODT, PDF); se citește doar dacă e nevoie.
    Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOp {
    Is,
    IsNot,
    Contains,
    DoesNotContain,
    IsNothing,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeOp {
    Larger,
    Smaller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Created,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOp {
    /// Înainte de un moment dat (secunde Unix).
    Before(i64),
    /// După un moment dat.
    After(i64),
    /// Mai vechi de N zile față de „acum”.
    OlderThanDays(u32),
    /// În ultimele N zile.
    WithinDays(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    /// Toate fișierele; permis doar la nivelul de sus.
    AllFiles,
    /// Toate condițiile următoare (și).
    All(Vec<Condition>),
    /// Oricare dintre condițiile următoare (sau).
    Any(Vec<Condition>),
    Text { field: TextField, op: TextOp, value: String },
    Size { op: SizeOp, bytes: u64 },
    Date { field: DateField, op: DateOp },
    /// Fișierul are același conținut cu unul mai vechi (`is: true`), sau nu. Originalul se
    /// caută în folderul fișierului și, în plus, în `within` (ex. unde au fost sortate
    /// descărcările de dinainte).
    Duplicate { is: bool, within: Vec<crate::WatchedFolder> },
    /// A user variable finds a value in the file (`found: true`), or does not. The variable
    /// is looked up by name in [`EvalCtx::vars`]; an unknown one finds nothing.
    Var { name: String, found: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleError {
    EmptyGroup,
    TooDeep,
    AllFilesNotTop,
    TopMustBeGroup,
    NoFolders,
    NoActions,
    /// După ștergere sau mutare în Coș nu mai rămâne niciun fișier pe care să lucreze o acțiune.
    ActionAfterRemoval,
}

/// Cine citește textul fișierelor pentru condițiile pe conținut (motorul, cu acces la disc).
/// Întoarce textul deja în minuscule; `None` = fișierul nu are text de citit.
pub trait ContentLoader {
    fn load(&self, meta: &FileMeta) -> Option<Arc<str>>;
    /// Un fișier mai vechi cu exact același conținut (originalul), din folderul fișierului
    /// sau din `within`.
    fn duplicate_of(&self, _meta: &FileMeta, _within: &[crate::WatchedFolder]) -> Option<std::path::PathBuf> {
        None
    }
    /// Rezultatul ținut minte al unei condiții pe conținut (`key` = [`content_key`]) pentru
    /// această versiune a fișierului (dimensiune, dată), dacă există.
    fn verdict(&self, _meta: &FileMeta, _key: u64) -> Option<bool> {
        None
    }
    /// Ține minte rezultatul unei condiții pe conținut.
    fn remember(&self, _meta: &FileMeta, _key: u64, _matched: bool) {}
}

/// Cheia stabilă (aceeași între versiuni de program) a unei condiții pe conținut: operația și
/// valoarea căutată. FNV-1a pe 64 de biți; prefixul `c1` e versiunea cheii.
pub fn content_key(op: TextOp, value: &str) -> u64 {
    let op = match op {
        TextOp::Is => 1u8,
        TextOp::IsNot => 2,
        TextOp::Contains => 3,
        TextOp::DoesNotContain => 4,
        TextOp::IsNothing => 5,
        TextOp::StartsWith => 6,
        TextOp::EndsWith => 7,
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"c1".iter().chain([op].iter()).chain(value.to_lowercase().as_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Contextul evaluării: momentul curent, pentru condițiile relative la „acum”, și
/// variabilele utilizatorului, pentru condițiile pe variabile.
#[derive(Debug, Clone, Copy)]
pub struct EvalCtx<'a> {
    pub now: i64,
    pub vars: Option<&'a std::collections::HashMap<String, crate::template::Dictionary>>,
}

impl EvalCtx<'_> {
    pub fn new(now: i64) -> EvalCtx<'static> {
        EvalCtx { now, vars: None }
    }
}

/// Un fișier pregătit pentru evaluare: câmpurile text în minuscule se calculează
/// o singură dată și doar dacă o condiție chiar le cere.
pub struct Prepared<'a> {
    pub meta: &'a FileMeta,
    name: OnceCell<String>,
    ext: OnceCell<String>,
    path: OnceCell<String>,
    folder: OnceCell<String>,
    loader: Option<&'a dyn ContentLoader>,
    content: OnceCell<Option<Arc<str>>>,
    /// Rezultatul căutării dublurii, pe set de foldere căutate.
    duplicate: std::cell::RefCell<Vec<(Vec<crate::WatchedFolder>, bool)>>,
}

impl<'a> Prepared<'a> {
    pub fn new(meta: &'a FileMeta) -> Self {
        Prepared {
            meta,
            name: OnceCell::new(),
            ext: OnceCell::new(),
            path: OnceCell::new(),
            folder: OnceCell::new(),
            loader: None,
            content: OnceCell::new(),
            duplicate: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Ca `new`, cu cine citește conținutul (pentru condițiile pe conținut).
    pub fn with_content(meta: &'a FileMeta, loader: &'a dyn ContentLoader) -> Self {
        Prepared { loader: Some(loader), ..Prepared::new(meta) }
    }

    fn field(&self, f: TextField) -> &str {
        match f {
            TextField::Name => self.name.get_or_init(|| self.meta.stem().to_lowercase()),
            TextField::Extension => self.ext.get_or_init(|| self.meta.extension().to_lowercase()),
            TextField::Path => self.path.get_or_init(|| self.meta.full_path().to_lowercase()),
            TextField::Folder => self.folder.get_or_init(|| self.meta.folder().to_lowercase()),
            TextField::Content => self
                .content
                .get_or_init(|| self.loader.and_then(|l| l.load(self.meta)))
                .as_deref()
                .unwrap_or(""),
        }
    }
}

fn text_matches(op: TextOp, hay_lower: &str, needle: &str) -> bool {
    // Valoarea căutată se transformă în minuscule doar dacă are majuscule.
    let n: Cow<str> = if needle.chars().any(char::is_uppercase) {
        Cow::Owned(needle.to_lowercase())
    } else {
        Cow::Borrowed(needle)
    };
    match op {
        TextOp::Is => hay_lower == n,
        TextOp::IsNot => hay_lower != n,
        TextOp::Contains => hay_lower.contains(&*n),
        TextOp::DoesNotContain => !hay_lower.contains(&*n),
        TextOp::IsNothing => hay_lower.is_empty(),
        TextOp::StartsWith => hay_lower.starts_with(&*n),
        TextOp::EndsWith => hay_lower.ends_with(&*n),
    }
}

impl Condition {
    /// Verifică structura: la nivelul de sus trebuie să fie un grup sau „Toate fișierele”,
    /// grupurile nu sunt goale și nu se imbrică peste [`MAX_DEPTH`].
    pub fn validate(&self) -> Result<(), RuleError> {
        match self {
            Condition::All(_) | Condition::Any(_) | Condition::AllFiles => {}
            _ => return Err(RuleError::TopMustBeGroup),
        }
        self.validate_inner(1, true)
    }

    fn validate_inner(&self, depth: usize, top: bool) -> Result<(), RuleError> {
        match self {
            Condition::AllFiles if top => Ok(()),
            Condition::AllFiles => Err(RuleError::AllFilesNotTop),
            Condition::All(c) | Condition::Any(c) => {
                if depth > MAX_DEPTH {
                    return Err(RuleError::TooDeep);
                }
                if c.is_empty() {
                    return Err(RuleError::EmptyGroup);
                }
                c.iter().try_for_each(|x| x.validate_inner(depth + 1, false))
            }
            _ => Ok(()),
        }
    }

    /// Costul relativ al testului: numele și extensia întâi, apoi calea, apoi
    /// dimensiunea și datele. Un grup costă cât cel mai scump copil al lui.
    fn cost(&self) -> u32 {
        match self {
            Condition::AllFiles => 0,
            Condition::Text { field: TextField::Name | TextField::Extension, .. } => 1,
            // Citirea fișierului: ultima, după toate testele pe metadate.
            Condition::Text { field: TextField::Content, .. } => 10,
            // Listarea folderului și compararea fișierelor de aceeași dimensiune.
            Condition::Duplicate { .. } => 9,
            // Possibly the file's text, and a search in it.
            Condition::Var { .. } => 10,
            Condition::Text { .. } => 2,
            Condition::Size { .. } | Condition::Date { .. } => 3,
            Condition::All(c) | Condition::Any(c) => c.iter().map(Condition::cost).max().unwrap_or(0),
        }
    }

    /// Reordonează copiii fiecărui grup de la ieftin la scump (sortare stabilă), ca un
    /// fișier să fie respins cât mai devreme. Rezultatul logic nu se schimbă.
    pub fn optimized(self) -> Condition {
        match self {
            Condition::All(c) => Condition::All(Self::sorted(c)),
            Condition::Any(c) => Condition::Any(Self::sorted(c)),
            other => other,
        }
    }

    fn sorted(children: Vec<Condition>) -> Vec<Condition> {
        let mut v: Vec<Condition> = children.into_iter().map(Condition::optimized).collect();
        v.sort_by_key(Condition::cost);
        v
    }

    pub fn matches(&self, p: &Prepared<'_>, ctx: &EvalCtx<'_>) -> bool {
        match self {
            Condition::AllFiles => true,
            Condition::All(c) => c.iter().all(|x| x.matches(p, ctx)),
            Condition::Any(c) => c.iter().any(|x| x.matches(p, ctx)),
            Condition::Text { field: TextField::Content, op, value } => {
                // Rezultatul ținut minte pentru această versiune a fișierului: textul unui PDF
                // nu se mai extrage la fiecare scanare.
                let key = content_key(*op, value);
                if let Some(v) = p.loader.and_then(|l| l.verdict(p.meta, key)) {
                    return v;
                }
                let v = text_matches(*op, p.field(TextField::Content), value);
                // Doar dacă textul s-a putut citi: altfel se încearcă din nou data viitoare.
                if p.content.get().is_some_and(Option::is_some) {
                    if let Some(l) = p.loader {
                        l.remember(p.meta, key, v);
                    }
                }
                v
            }
            Condition::Text { field, op, value } => {
                // Extensia se poate scrie și cu punct: „.pdf” = „pdf”.
                let v = if *field == TextField::Extension { value.trim_start_matches('.') } else { value };
                text_matches(*op, p.field(*field), v)
            }
            Condition::Duplicate { is, within } => {
                let known = p.duplicate.borrow().iter().find(|(w, _)| w == within).map(|(_, d)| *d);
                let dup = known.unwrap_or_else(|| {
                    let d = p.loader.is_some_and(|l| l.duplicate_of(p.meta, within).is_some());
                    p.duplicate.borrow_mut().push((within.clone(), d));
                    d
                });
                dup == *is
            }
            Condition::Var { name, found } => {
                let Some(dict) = ctx.vars.and_then(|v| v.get(name)) else { return !*found };
                let content = dict.field == TextField::Content;
                let key = dict.content_key();
                if content {
                    if let Some(v) = p.loader.and_then(|l| l.verdict(p.meta, key)) {
                        return v == *found;
                    }
                }
                // Finding a value does not depend on letter case: the lowercase text will do.
                let hit = dict.lookup(p.field(dict.field), crate::timefmt::Lang::En).ok().flatten().is_some();
                if content && p.content.get().is_some_and(Option::is_some) {
                    if let Some(l) = p.loader {
                        l.remember(p.meta, key, hit);
                    }
                }
                hit == *found
            }
            Condition::Size { op, bytes } => match op {
                SizeOp::Larger => p.meta.size > *bytes,
                SizeOp::Smaller => p.meta.size < *bytes,
            },
            Condition::Date { field, op } => {
                let t = match field {
                    DateField::Created => p.meta.created,
                    DateField::Modified => p.meta.modified,
                };
                match op {
                    DateOp::Before(x) => t < *x,
                    DateOp::After(x) => t > *x,
                    DateOp::OlderThanDays(n) => t < ctx.now - i64::from(*n) * DAY,
                    DateOp::WithinDays(n) => t >= ctx.now - i64::from(*n) * DAY,
                }
            }
        }
    }

    /// Rezultatul depinde de trecerea timpului („mai vechi de N zile”, „în ultimele N zile”)?
    /// Atunci un fișier neschimbat poate începe să se potrivească mai târziu, deci regula
    /// cere rescanări periodice.
    pub fn is_time_relative(&self) -> bool {
        match self {
            Condition::All(c) | Condition::Any(c) => c.iter().any(Condition::is_time_relative),
            Condition::Date { op: DateOp::OlderThanDays(_) | DateOp::WithinDays(_), .. } => true,
            _ => false,
        }
    }

    /// Condiția caută dubluri?
    pub fn uses_duplicate(&self) -> bool {
        match self {
            Condition::All(c) | Condition::Any(c) => c.iter().any(Condition::uses_duplicate),
            Condition::Duplicate { .. } => true,
            _ => false,
        }
    }

    /// Folderele în care regula caută originalul (prima condiție de dublură).
    pub fn duplicate_within(&self) -> Option<&[crate::WatchedFolder]> {
        match self {
            Condition::All(c) | Condition::Any(c) => c.iter().find_map(Condition::duplicate_within),
            Condition::Duplicate { within, .. } => Some(within),
            _ => None,
        }
    }

    /// Condiția are nevoie de textul fișierului?
    pub fn uses_content(&self) -> bool {
        match self {
            Condition::All(c) | Condition::Any(c) => c.iter().any(Condition::uses_content),
            Condition::Text { field: TextField::Content, .. } => true,
            // The variable may search the file's text; checking stability first is harmless.
            Condition::Var { .. } => true,
            _ => false,
        }
    }

    /// Scurtătură pentru un singur fișier.
    pub fn matches_meta(&self, meta: &FileMeta, ctx: &EvalCtx) -> bool {
        self.matches(&Prepared::new(meta), ctx)
    }
}
