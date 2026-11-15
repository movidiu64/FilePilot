//! Dublurile: catalogul amprentelor (persistent) și listările folderelor (pe o trecere).
//!
//! Un fișier e dublura altuia dacă au aceeași dimensiune și același conținut. Căutarea merge
//! în trepte, fiecare doar pe ce a trecut de cea dinainte:
//! 1. dimensiunea (din listare, gratis);
//! 2. amprenta primilor 64 KB;
//! 3. amprenta întregului fișier (doar unde primele două se potrivesc);
//! 4. înaintea unei acțiuni, comparația octet cu octet.
//!
//! Amprentele se păstrează în catalog cât timp dimensiunea și data modificării nu se schimbă,
//! deci un fișier mare se citește o singură dată, nu la fiecare scanare sau previzualizare.
//! Un catalog sau o listare învechită nu poate duce la o acțiune greșită: pasul 4 citește
//! discul, iar un fișier dispărut nu se potrivește cu nimic.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use org_core::{FileMeta, WatchedFolder};
use org_fs::{fingerprint, Fs, QUICK_BYTES};

use crate::is_temp_download;

/// Amprentele unui fișier, valabile cât timp dimensiunea și data modificării sunt acestea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintRow {
    pub size: u64,
    pub modified: i64,
    /// Amprenta primilor 64 KB (la fișierele mai mici, a întregului conținut).
    pub quick: Option<u64>,
    pub full: Option<u64>,
}

/// Rezultatul ținut minte al unei condiții pe conținut, pentru o versiune a fișierului.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictRow {
    pub size: u64,
    pub modified: i64,
    pub matched: bool,
}

/// Catalogul: amprentele (dubluri) și rezultatele condițiilor pe conținut, pe fișier. Daemonul
/// îl încarcă din baza de date la pornire și salvează schimbările după fiecare trecere.
#[derive(Debug, Default)]
pub struct Prints {
    rows: RefCell<HashMap<PathBuf, PrintRow>>,
    dirty: RefCell<HashSet<PathBuf>>,
    verdicts: RefCell<HashMap<(PathBuf, u64), VerdictRow>>,
    dirty_verdicts: RefCell<HashSet<(PathBuf, u64)>>,
}

impl Prints {
    pub fn from_rows(rows: impl IntoIterator<Item = (PathBuf, PrintRow)>) -> Self {
        Prints { rows: RefCell::new(rows.into_iter().collect()), ..Default::default() }
    }

    pub fn with_verdicts(self, rows: impl IntoIterator<Item = ((PathBuf, u64), VerdictRow)>) -> Self {
        *self.verdicts.borrow_mut() = rows.into_iter().collect();
        self
    }

    /// Rezultatul ținut minte, dacă e pentru aceeași versiune a fișierului.
    pub fn verdict(&self, p: &Path, size: u64, modified: i64, key: u64) -> Option<bool> {
        let v = self.verdicts.borrow();
        let r = v.get(&(p.to_path_buf(), key))?;
        (r.size == size && r.modified == modified).then_some(r.matched)
    }

    pub fn remember(&self, p: &Path, size: u64, modified: i64, key: u64, matched: bool) {
        let k = (p.to_path_buf(), key);
        let row = VerdictRow { size, modified, matched };
        if self.verdicts.borrow().get(&k) == Some(&row) {
            return;
        }
        self.verdicts.borrow_mut().insert(k.clone(), row);
        self.dirty_verdicts.borrow_mut().insert(k);
    }

    /// Rezultatele noi sau schimbate de la ultimul apel.
    pub fn take_verdict_changes(&self) -> Vec<((PathBuf, u64), VerdictRow)> {
        let dirty: Vec<(PathBuf, u64)> = self.dirty_verdicts.borrow_mut().drain().collect();
        let v = self.verdicts.borrow();
        dirty.into_iter().filter_map(|k| v.get(&k).map(|r| (k, *r))).collect()
    }

    pub fn verdict_count(&self) -> usize {
        self.verdicts.borrow().len()
    }

    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Rândurile noi sau schimbate de la ultimul apel.
    pub fn take_changes(&self) -> Vec<(PathBuf, PrintRow)> {
        let dirty: Vec<PathBuf> = self.dirty.borrow_mut().drain().collect();
        let rows = self.rows.borrow();
        dirty.into_iter().filter_map(|p| rows.get(&p).map(|r| (p, *r))).collect()
    }

    /// Scoate rândurile (amprente și rezultate) fișierelor care nu mai există; le întoarce,
    /// ca să fie șterse și din baza de date.
    pub fn remove_missing(&self, fs: &dyn Fs) -> Vec<PathBuf> {
        let mut paths: HashSet<PathBuf> = self.rows.borrow().keys().cloned().collect();
        paths.extend(self.verdicts.borrow().keys().map(|(p, _)| p.clone()));
        let mut gone: Vec<PathBuf> = paths.into_iter().filter(|p| !fs.exists(p)).collect();
        gone.sort();
        let set: HashSet<&PathBuf> = gone.iter().collect();
        let mut rows = self.rows.borrow_mut();
        let mut dirty = self.dirty.borrow_mut();
        for p in &gone {
            rows.remove(p);
            dirty.remove(p);
        }
        self.verdicts.borrow_mut().retain(|(p, _), _| !set.contains(p));
        self.dirty_verdicts.borrow_mut().retain(|(p, _)| !set.contains(p));
        gone
    }

    /// Rândul valabil pentru această versiune a fișierului (sau unul gol).
    fn current(&self, p: &Path, size: u64, modified: i64) -> PrintRow {
        match self.rows.borrow().get(p) {
            Some(r) if r.size == size && r.modified == modified => *r,
            _ => PrintRow { size, modified, quick: None, full: None },
        }
    }

    fn put(&self, p: &Path, row: PrintRow) {
        self.rows.borrow_mut().insert(p.to_path_buf(), row);
        self.dirty.borrow_mut().insert(p.to_path_buf());
    }

    /// Amprenta primilor 64 KB; `None` dacă fișierul nu se poate citi.
    pub fn quick(&self, fs: &dyn Fs, p: &Path, size: u64, modified: i64) -> Option<u64> {
        let mut row = self.current(p, size, modified);
        if let Some(q) = row.quick {
            return Some(q);
        }
        let data = fs.read_prefix(p, QUICK_BYTES).ok()?;
        // Fișierul s-a schimbat între listare și citire: nu se memorează nimic.
        if data.len() as u64 != size.min(QUICK_BYTES) {
            return None;
        }
        let q = fingerprint(&data);
        row.quick = Some(q);
        if size <= QUICK_BYTES {
            // Primii 64 KB sunt tot fișierul.
            row.full = Some(q);
        }
        self.put(p, row);
        Some(q)
    }

    /// Amprenta întregului conținut; `None` dacă fișierul nu se poate citi.
    pub fn full(&self, fs: &dyn Fs, p: &Path, size: u64, modified: i64) -> Option<u64> {
        if size <= QUICK_BYTES {
            return self.quick(fs, p, size, modified);
        }
        let mut row = self.current(p, size, modified);
        if let Some(f) = row.full {
            return Some(f);
        }
        let f = fs.hash_file(p).ok()?;
        row.full = Some(f);
        self.put(p, row);
        Some(f)
    }
}

/// Un fișier care poate fi original sau dublură.
#[derive(Debug, Clone)]
struct Cand {
    created: i64,
    /// `raport.pdf` înaintea lui `raport (1).pdf`: numele mai scurt, apoi alfabetic.
    key: (usize, String),
    path: PathBuf,
    size: u64,
    modified: i64,
}

impl Cand {
    /// Ordinea în care se alege originalul: cel creat primul, apoi numele.
    fn order(&self) -> (i64, &(usize, String), &Path) {
        (self.created, &self.key, &self.path)
    }
}

fn name_key(p: &Path) -> (usize, String) {
    let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    (n.chars().count(), n)
}

/// Fișierele unui folder (după dimensiune) și subfolderele lui.
#[derive(Debug, Default)]
struct Listing {
    by_size: HashMap<u64, Vec<Cand>>,
    subdirs: Vec<PathBuf>,
}

/// Listările folderelor, ținute pe o trecere (scanare, previzualizare, căutarea din pagina
/// Dubluri): fără ele, fiecare fișier ar relista folderele (pătratic). Plus catalogul.
#[derive(Default)]
pub struct DirCache {
    dirs: RefCell<HashMap<PathBuf, Option<Rc<Listing>>>>,
    prints: Rc<Prints>,
}

impl DirCache {
    pub fn new(prints: Rc<Prints>) -> Self {
        DirCache { dirs: RefCell::default(), prints }
    }

    pub fn prints(&self) -> &Prints {
        &self.prints
    }

    fn listing(&self, fs: &dyn Fs, dir: &Path) -> Option<Rc<Listing>> {
        if let Some(l) = self.dirs.borrow().get(dir) {
            return l.clone();
        }
        let l = fs.list_dir(dir).ok().map(|entries| {
            let mut l = Listing::default();
            for e in entries {
                if e.is_dir {
                    l.subdirs.push(e.path);
                    continue;
                }
                if is_temp_download(&e.path) {
                    continue;
                }
                let Some((st, created)) = e.stat.or_else(|| fs.stat_full(&e.path).ok()) else { continue };
                let key = name_key(&e.path);
                l.by_size.entry(st.size).or_default().push(Cand { created, key, path: e.path, size: st.size, modified: st.modified });
            }
            Rc::new(l)
        });
        self.dirs.borrow_mut().insert(dir.to_path_buf(), l.clone());
        l
    }

    /// Toate listările dintr-un folder (cu subfolderele, dacă `recursive`).
    fn walk(&self, fs: &dyn Fs, root: &Path, recursive: bool, out: &mut Vec<Rc<Listing>>) {
        let mut stack = vec![root.to_path_buf()];
        let mut seen = HashSet::new();
        while let Some(dir) = stack.pop() {
            if !seen.insert(dir.clone()) {
                continue;
            }
            let Some(l) = self.listing(fs, &dir) else { continue };
            if recursive {
                stack.extend(l.subdirs.iter().cloned());
            }
            out.push(l);
        }
    }

    /// Originalul lui `meta`: cel mai vechi fișier cu același conținut, din folderul lui sau
    /// din `within`. `verify`: confirmarea finală octet cu octet (înaintea acțiunilor);
    /// fără ea (previzualizare, liste) decid amprentele.
    pub fn original(&self, fs: &dyn Fs, meta: &FileMeta, within: &[WatchedFolder], verify: bool) -> Option<PathBuf> {
        let mut lists = Vec::new();
        if let Some(parent) = meta.path.parent() {
            self.walk(fs, parent, false, &mut lists);
        }
        for w in within.iter().filter(|w| !w.path.as_os_str().is_empty()) {
            self.walk(fs, &w.path, w.recursive, &mut lists);
        }
        let me = Cand {
            created: meta.created,
            key: name_key(&meta.path),
            path: meta.path.clone(),
            size: meta.size,
            modified: meta.modified,
        };
        let mut seen = HashSet::new();
        let mut older: Vec<&Cand> = lists
            .iter()
            .filter_map(|l| l.by_size.get(&meta.size))
            .flatten()
            .filter(|c| c.path != meta.path && c.order() < me.order() && seen.insert(&c.path))
            .collect();
        if older.is_empty() {
            return None;
        }
        older.sort_by(|a, b| a.order().cmp(&b.order()));
        let pr = &self.prints;
        let mine_quick = pr.quick(fs, &meta.path, meta.size, meta.modified);
        let mut mine_full: Option<Option<u64>> = None;
        for c in older {
            if let Some(q) = mine_quick {
                // Amprente diferite exclud sigur; necitibil = decide comparația completă.
                if pr.quick(fs, &c.path, c.size, c.modified).is_some_and(|x| x != q) {
                    continue;
                }
                let mine = *mine_full.get_or_insert_with(|| pr.full(fs, &meta.path, meta.size, meta.modified));
                let theirs = pr.full(fs, &c.path, c.size, c.modified);
                if let (Some(a), Some(b)) = (mine, theirs) {
                    if a != b {
                        continue;
                    }
                    if !verify {
                        return Some(c.path.clone());
                    }
                }
            }
            if fs.same_content(&c.path, &meta.path).unwrap_or(false) {
                return Some(c.path.clone());
            }
        }
        None
    }

    /// Grupurile de fișiere identice din `folders` (pagina Dubluri), cel puțin de
    /// `min_size` octeți. Primul fișier din grup e originalul (cel mai vechi). `progress`
    /// primește (pas, gata, total) și întoarce `false` ca să oprească.
    pub fn groups(
        &self,
        fs: &dyn Fs,
        folders: &[WatchedFolder],
        min_size: u64,
        progress: &mut dyn FnMut(GroupStep, usize, usize) -> bool,
    ) -> Option<Vec<DupGroup>> {
        // 1. Listarea: fișierele de aceeași dimensiune.
        let mut lists = Vec::new();
        for f in folders.iter().filter(|w| !w.path.as_os_str().is_empty()) {
            self.walk(fs, &f.path, f.recursive, &mut lists);
            if !progress(GroupStep::Listing, lists.len(), 0) {
                return None;
            }
        }
        let mut by_size: HashMap<u64, Vec<&Cand>> = HashMap::new();
        let mut seen = HashSet::new();
        for c in lists.iter().flat_map(|l| l.by_size.iter()).filter(|(s, _)| **s >= min_size.max(1)).flat_map(|(_, v)| v) {
            if seen.insert(&c.path) {
                by_size.entry(c.size).or_default().push(c);
            }
        }
        by_size.retain(|_, v| v.len() > 1);
        // 2. Amprenta rapidă, apoi 3. cea completă, doar pe ce s-a potrivit înainte.
        let total: usize = by_size.values().map(Vec::len).sum();
        let mut done = 0;
        let mut quick_groups: Vec<Vec<&Cand>> = Vec::new();
        for v in by_size.into_values() {
            let mut by_quick: HashMap<u64, Vec<&Cand>> = HashMap::new();
            for c in v {
                if let Some(q) = self.prints.quick(fs, &c.path, c.size, c.modified) {
                    by_quick.entry(q).or_default().push(c);
                }
                done += 1;
                if done % 16 == 0 && !progress(GroupStep::Quick, done, total) {
                    return None;
                }
            }
            quick_groups.extend(by_quick.into_values().filter(|g| g.len() > 1));
        }
        let total: usize = quick_groups.iter().map(Vec::len).sum();
        let mut done = 0;
        let mut out = Vec::new();
        for g in quick_groups {
            let mut by_full: HashMap<u64, Vec<&Cand>> = HashMap::new();
            for c in g {
                if let Some(f) = self.prints.full(fs, &c.path, c.size, c.modified) {
                    by_full.entry(f).or_default().push(c);
                }
                done += 1;
                if !progress(GroupStep::Full, done, total) {
                    return None;
                }
            }
            for mut v in by_full.into_values().filter(|g| g.len() > 1) {
                v.sort_by(|a, b| a.order().cmp(&b.order()));
                out.push(DupGroup {
                    size: v[0].size,
                    files: v.iter().map(|c| DupFile { path: c.path.clone(), created: c.created, modified: c.modified }).collect(),
                });
            }
        }
        // Întâi grupurile care eliberează cel mai mult spațiu.
        out.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then_with(|| a.files[0].path.cmp(&b.files[0].path)));
        Some(out)
    }
}

/// Pasul căutării din pagina Dubluri, pentru progres.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupStep {
    Listing,
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupFile {
    pub path: PathBuf,
    pub created: i64,
    pub modified: i64,
}

/// Fișiere cu același conținut; primul e originalul.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupGroup {
    pub size: u64,
    pub files: Vec<DupFile>,
}

impl DupGroup {
    /// Spațiul eliberat dacă rămâne doar originalul.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
    }
}
