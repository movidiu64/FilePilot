use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::{DirEntry, FileStat, Fs};

/// Sistem de fișiere „pe hârtie”, pentru previzualizare: citește din `inner`, iar
/// scrierile (mutări, copieri, ștergeri, foldere noi) rămân doar în memorie. Comenzile
/// nu se rulează. Acțiunile trec prin exact aceeași logică (conflicte, `_2`, foldere),
/// deci rezultatul arătat e cel pe care l-ar produce daemonul.
pub struct DryFs<'a> {
    inner: &'a dyn Fs,
    added: HashMap<PathBuf, FileStat>,
    removed: HashSet<PathBuf>,
    dirs: HashSet<PathBuf>,
    /// De unde vine un fișier „apărut” (calea reală de pe disc), pentru comparări de conținut.
    origin: HashMap<PathBuf, PathBuf>,
}

impl<'a> DryFs<'a> {
    pub fn new(inner: &'a dyn Fs) -> Self {
        DryFs { inner, added: HashMap::new(), removed: HashSet::new(), dirs: HashSet::new(), origin: HashMap::new() }
    }

    /// Calea reală din spatele unei căi din simulare.
    fn real(&self, p: &Path) -> PathBuf {
        let mut cur = p.to_path_buf();
        for _ in 0..64 {
            match self.origin.get(&cur) {
                Some(o) if self.added.contains_key(&cur) => cur = o.clone(),
                _ => break,
            }
        }
        cur
    }

    fn gone(&mut self, p: &Path) -> io::Result<()> {
        if !self.exists(p) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "fișierul nu există"));
        }
        self.added.remove(p);
        self.removed.insert(p.to_path_buf());
        Ok(())
    }

    fn appear(&mut self, p: &Path, st: FileStat) {
        self.removed.remove(p);
        self.added.insert(p.to_path_buf(), st);
    }
}

impl Fs for DryFs<'_> {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        let mut v: Vec<DirEntry> = match self.inner.list_dir(dir) {
            Ok(v) => v.into_iter().filter(|e| !self.removed.contains(&e.path)).collect(),
            Err(_) if self.dirs.contains(dir) => Vec::new(),
            Err(e) => return Err(e),
        };
        for (p, st) in &self.added {
            if p.parent() == Some(dir) && !v.iter().any(|e| &e.path == p) {
                v.push(DirEntry { path: p.clone(), is_dir: false, stat: Some((*st, st.modified)) });
            }
        }
        Ok(v)
    }

    fn created(&self, p: &Path) -> io::Result<i64> {
        if let Some(st) = self.added.get(p) {
            return Ok(st.modified);
        }
        self.metadata(p)?;
        self.inner.created(p)
    }

    fn in_use(&self, p: &Path) -> bool {
        self.inner.in_use(p)
    }

    fn read_prefix(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
        if self.removed.contains(p) || self.added.contains_key(p) {
            // Fișierele „mutate” în simulare nu există încă pe disc.
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        self.inner.read_prefix(p, max)
    }

    fn exists(&self, p: &Path) -> bool {
        if self.added.contains_key(p) || self.dirs.contains(p) {
            return true;
        }
        !self.removed.contains(p) && self.inner.exists(p)
    }

    fn metadata(&self, p: &Path) -> io::Result<FileStat> {
        if let Some(st) = self.added.get(p) {
            return Ok(*st);
        }
        if self.removed.contains(p) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "fișierul nu există"));
        }
        self.inner.metadata(p)
    }

    fn create_dir_all(&mut self, p: &Path) -> io::Result<()> {
        self.dirs.insert(p.to_path_buf());
        Ok(())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        // Only folders created in the simulation itself (e.g. an extraction) can be renamed.
        if self.dirs.remove(from) {
            self.dirs.insert(to.to_path_buf());
            return Ok(());
        }
        let st = self.metadata(from)?;
        let real = self.real(from);
        self.gone(from)?;
        self.appear(to, st);
        self.origin.insert(to.to_path_buf(), real);
        Ok(())
    }

    fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64> {
        let st = self.metadata(from)?;
        let real = self.real(from);
        self.appear(to, st);
        self.origin.insert(to.to_path_buf(), real);
        Ok(st.size)
    }

    fn same_content(&self, a: &Path, b: &Path) -> io::Result<bool> {
        if self.metadata(a)?.size != self.metadata(b)?.size {
            return Ok(false);
        }
        let (ra, rb) = (self.real(a), self.real(b));
        if ra == rb {
            return Ok(true);
        }
        self.inner.same_content(&ra, &rb)
    }

    fn remove_file(&mut self, p: &Path) -> io::Result<()> {
        self.gone(p)
    }

    fn trash(&mut self, p: &Path) -> io::Result<()> {
        self.gone(p)
    }

    fn restore_from_trash(&mut self, _original: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "previzualizare"))
    }

    fn remove_dir_if_empty(&mut self, _p: &Path) -> io::Result<bool> {
        Ok(false)
    }

    /// În previzualizare nu se rulează nimic.
    fn run(&mut self, _program: &str, _args: &[String]) -> io::Result<i32> {
        Ok(0)
    }

    fn run_shell(&mut self, _line: &str) -> io::Result<i32> {
        Ok(0)
    }

    /// The preview does not open archives: the folder stays empty.
    fn unpack(&mut self, archive: &Path, into: &Path) -> io::Result<()> {
        self.metadata(archive).map(|_| ())?;
        if self.dirs.contains(into) { Ok(()) } else { Err(io::Error::from(io::ErrorKind::NotFound)) }
    }
}
