//! Abstracția sistemului de fișiere: implementarea reală (`StdFs`) și una în memorie
//! (`MemFs`) cu injecție de erori, pentru testarea recuperării după întreruperi.

use std::io;
use std::path::{Path, PathBuf};

mod dry;
mod mem;
mod print;
mod std_fs;

pub use dry::DryFs;
pub use mem::MemFs;
pub use print::{fingerprint, Hasher64, PRINT_VERSION, QUICK_BYTES};
pub use std_fs::StdFs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    /// Secunde Unix.
    pub modified: i64,
}

/// Operațiile pe care le au nevoie acțiunile. Toate sunt sincrone și simple, ca să
/// poată fi înlocuite în teste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    /// (dimensiune + dată modificare, dată creare), când listarea le dă fără citiri în plus
    /// (pe Windows vin din căutarea în director). Pot fi ușor în urmă pentru un fișier încă
    /// în scriere: bune pentru condiții, dar înainte de acțiuni se recitesc.
    pub stat: Option<(FileStat, i64)>,
}

pub trait Fs {
    /// Conținutul direct al unui folder (fără recursie). Linkurile simbolice nu se urmează.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>>;
    /// Data creării (secunde Unix); implicit data modificării.
    fn created(&self, p: &Path) -> io::Result<i64> {
        self.metadata(p).map(|s| s.modified)
    }
    /// Dimensiune, dată modificare și dată creare, dintr-o singură citire dacă se poate.
    fn stat_full(&self, p: &Path) -> io::Result<(FileStat, i64)> {
        let s = self.metadata(p)?;
        Ok((s, self.created(p).unwrap_or(s.modified)))
    }
    fn exists(&self, p: &Path) -> bool;
    /// Fișierul e deschis acum de alt program (Windows: deschiderea exclusivă e refuzată)?
    /// Unde nu se poate afla, `false`.
    fn in_use(&self, p: &Path) -> bool;
    /// Cele două fișiere au exact același conținut? (dimensiunea întâi, apoi octet cu octet)
    fn same_content(&self, a: &Path, b: &Path) -> io::Result<bool> {
        let (ma, mb) = (self.metadata(a)?, self.metadata(b)?);
        if ma.size != mb.size {
            return Ok(false);
        }
        Ok(self.read_prefix(a, ma.size)? == self.read_prefix(b, mb.size)?)
    }
    /// Primii `max` octeți ai fișierului (pentru condițiile pe conținut).
    fn read_prefix(&self, p: &Path, max: u64) -> io::Result<Vec<u8>>;
    /// Amprenta întregului conținut ([`fingerprint`]); pe disc se citește pe bucăți.
    fn hash_file(&self, p: &Path) -> io::Result<u64> {
        Ok(fingerprint(&self.read_prefix(p, u64::MAX)?))
    }
    fn metadata(&self, p: &Path) -> io::Result<FileStat>;
    fn create_dir_all(&mut self, p: &Path) -> io::Result<()>;
    /// Redenumire în interiorul aceluiași volum. Între volume trebuie să eșueze cu
    /// [`io::ErrorKind::CrossesDevices`].
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64>;
    fn remove_file(&mut self, p: &Path) -> io::Result<()>;
    /// Mută în Coșul de reciclare.
    fn trash(&mut self, p: &Path) -> io::Result<()>;
    /// Readuce din Coș un fișier care a avut calea `original`.
    fn restore_from_trash(&mut self, original: &Path) -> io::Result<()>;
    /// Șterge folderul dacă e gol; întoarce dacă l-a șters.
    fn remove_dir_if_empty(&mut self, p: &Path) -> io::Result<bool>;
    /// Rulează un program direct, fără shell. Întoarce codul de ieșire.
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<i32>;
    /// Rulează o linie de comandă prin shell (`cmd /D /S /C` pe Windows, `sh -c` în rest).
    fn run_shell(&mut self, line: &str) -> io::Result<i32>;
}

/// Primul nume liber: `invoice.docx` devine `invoice_2.docx`, apoi `invoice_3.docx`.
pub fn unique_path(fs: &dyn Fs, p: &Path) -> PathBuf {
    if !fs.exists(p) {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    let mut n: u64 = 2;
    loop {
        let name = match &ext {
            Some(e) => format!("{stem}_{n}.{e}"),
            None => format!("{stem}_{n}"),
        };
        let candidate = p.with_file_name(name);
        if !fs.exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Mută un fișier. Între volume, redenumirea eșuează, deci se face copiere, verificare
/// a dimensiunii și abia apoi ștergerea sursei: o oprire la mijloc lasă mereu sursa intactă.
pub fn move_file(fs: &mut dyn Fs, from: &Path, to: &Path) -> io::Result<()> {
    match fs.rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            let existed = fs.exists(to);
            let size = fs.metadata(from)?.size;
            if let Err(e) = fs.copy(from, to) {
                if !existed {
                    let _ = fs.remove_file(to);
                }
                return Err(e);
            }
            let copied = fs.metadata(to).map(|s| s.size).unwrap_or(u64::MAX);
            if copied != size {
                if !existed {
                    let _ = fs.remove_file(to);
                }
                return Err(io::Error::other("verificarea copiei a eșuat"));
            }
            fs.remove_file(from)
        }
        other => other,
    }
}

/// Șterge folderele goale de la `start` în sus, oprindu-se la `root` (care rămâne).
pub fn remove_empty_parents(fs: &mut dyn Fs, start: &Path, root: &Path) {
    let mut dir = start.to_path_buf();
    while dir != root && dir.starts_with(root) {
        match fs.remove_dir_if_empty(&dir) {
            Ok(true) => match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            },
            _ => break,
        }
    }
}
