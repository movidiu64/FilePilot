use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::{DirEntry, FileStat, Fs};

/// Sistem de fișiere în memorie, pentru teste.
///
/// Poate simula volume diferite (redenumirea între ele eșuează cu `CrossesDevices`),
/// o operație care eșuează la un moment ales și o copiere întreruptă la jumătate.
#[derive(Debug, Default)]
pub struct MemFs {
    files: BTreeMap<PathBuf, FileStat>,
    dirs: BTreeSet<PathBuf>,
    trash: BTreeMap<PathBuf, FileStat>,
    trashed_dirs: BTreeSet<PathBuf>,
    volumes: Vec<PathBuf>,
    ops: usize,
    fail_at: Option<usize>,
    torn_copy: bool,
    /// Programele rulate, cu argumentele lor.
    pub ran: Vec<(String, Vec<String>)>,
    /// Codul de ieșire întors de `run`.
    pub run_exit: i32,
    contents: BTreeMap<PathBuf, Vec<u8>>,
    /// De câte ori s-a citit conținutul unui fișier.
    pub reads: std::cell::Cell<usize>,
    /// De câte ori s-a listat un folder.
    pub lists: std::cell::Cell<usize>,
    /// Fișiere „deschise de alt program”.
    pub busy: std::collections::BTreeSet<PathBuf>,
}

fn not_found() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, p: impl AsRef<Path>, size: u64, modified: i64) {
        let p = p.as_ref();
        self.add_ancestors(p);
        self.files.insert(p.to_path_buf(), FileStat { size, modified });
    }

    /// Fișier cu conținut (dimensiunea = lungimea conținutului).
    pub fn add_file_with(&mut self, p: impl AsRef<Path>, content: &[u8], modified: i64) {
        self.add_file(&p, content.len() as u64, modified);
        self.contents.insert(p.as_ref().to_path_buf(), content.to_vec());
    }

    pub fn add_dir(&mut self, p: impl AsRef<Path>) {
        let p = p.as_ref();
        self.dirs.insert(p.to_path_buf());
        self.add_ancestors(p);
    }

    fn add_ancestors(&mut self, p: &Path) {
        for a in p.ancestors().skip(1) {
            self.dirs.insert(a.to_path_buf());
        }
    }

    pub fn stat(&self, p: impl AsRef<Path>) -> Option<FileStat> {
        self.files.get(p.as_ref()).copied()
    }

    pub fn file_paths(&self) -> Vec<PathBuf> {
        self.files.keys().cloned().collect()
    }

    pub fn trashed(&self) -> Vec<PathBuf> {
        self.trash.keys().cloned().collect()
    }

    pub fn has_dir(&self, p: impl AsRef<Path>) -> bool {
        self.dirs.contains(p.as_ref())
    }

    /// Declară volume: redenumirea între două volume diferite eșuează cu `CrossesDevices`.
    pub fn set_volumes(&mut self, roots: &[&str]) {
        self.volumes = roots.iter().map(PathBuf::from).collect();
    }

    /// A `n`-a operație care modifică fișiere (numărând de la 0) eșuează o singură dată.
    pub fn fail_at(&mut self, n: usize) {
        self.fail_at = Some(n);
    }

    pub fn clear_failure(&mut self) {
        self.fail_at = None;
        self.torn_copy = false;
    }

    /// Următoarea copiere scrie jumătate din octeți și eșuează.
    pub fn torn_next_copy(&mut self) {
        self.torn_copy = true;
    }

    /// Numărul de operații care modifică fișiere, de până acum.
    pub fn ops(&self) -> usize {
        self.ops
    }

    fn volume_of(&self, p: &Path) -> Option<usize> {
        self.volumes.iter().position(|v| p.starts_with(v))
    }

    fn tick(&mut self) -> io::Result<()> {
        let n = self.ops;
        self.ops += 1;
        if self.fail_at == Some(n) {
            self.fail_at = None;
            return Err(io::Error::other("injected failure"));
        }
        Ok(())
    }

    fn parent_exists(&self, p: &Path) -> bool {
        match p.parent() {
            None => true,
            Some(par) if par.as_os_str().is_empty() => true,
            Some(par) => self.dirs.contains(par),
        }
    }
}

impl Fs for MemFs {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        self.lists.set(self.lists.get() + 1);
        if !self.dirs.contains(dir) {
            return Err(not_found());
        }
        let mut out = Vec::new();
        for f in self.files.keys().filter(|f| f.parent() == Some(dir)) {
            out.push(DirEntry { path: f.clone(), is_dir: false, stat: None });
        }
        for d in self.dirs.iter().filter(|d| d.parent() == Some(dir)) {
            out.push(DirEntry { path: d.clone(), is_dir: true, stat: None });
        }
        Ok(out)
    }

    fn in_use(&self, p: &Path) -> bool {
        self.busy.contains(p)
    }

    fn read_prefix(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
        if !self.files.contains_key(p) {
            return Err(not_found());
        }
        self.reads.set(self.reads.get() + 1);
        let c = self.contents.get(p).map_or(&[][..], Vec::as_slice);
        Ok(c[..c.len().min(max as usize)].to_vec())
    }

    fn exists(&self, p: &Path) -> bool {
        self.files.contains_key(p) || self.dirs.contains(p)
    }

    fn metadata(&self, p: &Path) -> io::Result<FileStat> {
        self.files.get(p).copied().ok_or_else(not_found)
    }

    fn create_dir_all(&mut self, p: &Path) -> io::Result<()> {
        self.add_dir(p);
        Ok(())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.tick()?;
        if self.dirs.contains(from) {
            return self.rename_dir(from, to);
        }
        let st = *self.files.get(from).ok_or_else(not_found)?;
        if self.volume_of(from) != self.volume_of(to) {
            return Err(io::Error::from(io::ErrorKind::CrossesDevices));
        }
        if !self.parent_exists(to) {
            return Err(not_found());
        }
        self.files.remove(from);
        self.files.insert(to.to_path_buf(), st);
        if let Some(c) = self.contents.remove(from) {
            self.contents.insert(to.to_path_buf(), c);
        }
        Ok(())
    }

    fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64> {
        self.tick()?;
        let st = *self.files.get(from).ok_or_else(not_found)?;
        if !self.parent_exists(to) {
            return Err(not_found());
        }
        if self.torn_copy {
            self.torn_copy = false;
            self.files.insert(to.to_path_buf(), FileStat { size: st.size / 2, modified: st.modified });
            return Err(io::Error::other("injected torn copy"));
        }
        self.files.insert(to.to_path_buf(), st);
        if let Some(c) = self.contents.get(from).cloned() {
            self.contents.insert(to.to_path_buf(), c);
        }
        Ok(st.size)
    }

    fn remove_file(&mut self, p: &Path) -> io::Result<()> {
        self.tick()?;
        self.files.remove(p).map(|_| ()).ok_or_else(not_found)
    }

    fn trash(&mut self, p: &Path) -> io::Result<()> {
        self.tick()?;
        if self.dirs.contains(p) {
            // A folder goes to the bin with everything in it.
            let inside: Vec<PathBuf> = self.files.keys().filter(|f| f.starts_with(p)).cloned().collect();
            for f in inside {
                let st = self.files.remove(&f).expect("listed");
                self.trash.insert(f, st);
            }
            self.dirs.retain(|d| !d.starts_with(p));
            self.trashed_dirs.insert(p.to_path_buf());
            return Ok(());
        }
        let st = self.files.remove(p).ok_or_else(not_found)?;
        self.trash.insert(p.to_path_buf(), st);
        Ok(())
    }

    fn restore_from_trash(&mut self, original: &Path) -> io::Result<()> {
        self.tick()?;
        if self.trashed_dirs.remove(original) {
            let inside: Vec<PathBuf> = self.trash.keys().filter(|f| f.starts_with(original)).cloned().collect();
            self.add_dir(original);
            for f in inside {
                let st = self.trash.remove(&f).expect("listed");
                self.add_ancestors(&f);
                self.files.insert(f, st);
            }
            return Ok(());
        }
        let st = self.trash.remove(original).ok_or_else(not_found)?;
        self.add_ancestors(original);
        self.files.insert(original.to_path_buf(), st);
        Ok(())
    }

    fn remove_dir_if_empty(&mut self, p: &Path) -> io::Result<bool> {
        if !self.dirs.contains(p) {
            return Ok(false);
        }
        let occupied = self.files.keys().any(|f| f.starts_with(p))
            || self.dirs.iter().any(|d| d != p && d.starts_with(p));
        if occupied {
            return Ok(false);
        }
        self.dirs.remove(p);
        Ok(true)
    }

    fn run(&mut self, program: &str, args: &[String]) -> io::Result<i32> {
        self.tick()?;
        self.ran.push((program.to_string(), args.to_vec()));
        Ok(self.run_exit)
    }

    fn run_shell(&mut self, line: &str) -> io::Result<i32> {
        self.tick()?;
        self.ran.push(("<shell>".to_string(), vec![line.to_string()]));
        Ok(self.run_exit)
    }

    /// Test archives are text: one entry per line, `path=content`, or `path/` for a folder.
    /// An archive whose text is `BROKEN` cannot be read; an entry with `..` is refused.
    fn unpack(&mut self, archive: &Path, into: &Path) -> io::Result<()> {
        self.tick()?;
        if !self.dirs.contains(into) {
            return Err(not_found());
        }
        let text = String::from_utf8_lossy(&self.read_prefix(archive, u64::MAX)?).into_owned();
        if text.trim() == "BROKEN" {
            return Err(io::Error::other("Unrecognized archive format"));
        }
        let modified = self.metadata(archive)?.modified;
        for line in text.lines().filter(|l| !l.is_empty()) {
            let (name, content) = line.split_once('=').unwrap_or((line, ""));
            if name.split('/').any(|c| c == "..") || name.starts_with('/') {
                return Err(io::Error::other(format!("Path contains '..': {name}")));
            }
            let p = into.join(name.trim_end_matches('/'));
            if name.ends_with('/') {
                self.add_dir(&p);
            } else {
                self.add_file_with(&p, content.as_bytes(), modified);
            }
        }
        Ok(())
    }
}

impl MemFs {
    fn rename_dir(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        if self.exists(to) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        if !self.parent_exists(to) {
            return Err(not_found());
        }
        let moved = |p: &Path| to.join(p.strip_prefix(from).expect("inside"));
        let files: Vec<PathBuf> = self.files.keys().filter(|f| f.starts_with(from)).cloned().collect();
        for f in files {
            let st = self.files.remove(&f).expect("listed");
            if let Some(c) = self.contents.remove(&f) {
                self.contents.insert(moved(&f), c);
            }
            self.files.insert(moved(&f), st);
        }
        let dirs: Vec<PathBuf> = self.dirs.iter().filter(|d| d.starts_with(from)).cloned().collect();
        for d in dirs {
            self.dirs.remove(&d);
            self.dirs.insert(moved(&d));
        }
        Ok(())
    }

    /// Folders sent to the bin as a whole.
    pub fn trashed_dirs(&self) -> Vec<PathBuf> {
        self.trashed_dirs.iter().cloned().collect()
    }
}
