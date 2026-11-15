//! Sistemul de fișiere al programului real: `StdFs` pentru tot, plus Coșul de reciclare
//! al sistemului (prin crate-ul `trash`), ca ștergerea să fie recuperabilă.

use std::io;
use std::path::Path;

use org_fs::{DirEntry, FileStat, Fs, StdFs};

#[derive(Debug, Default, Clone, Copy)]
pub struct SysFs;

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Comparație de căi tolerantă la majuscule și la separatorul final (Windows).
/// Calea ca text comparabil: fără `\\?\`, fără separator final, cu litere mici.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.trim_end_matches(['\\', '/']).to_lowercase()
}

/// Folderul din Coș e ținut cu numele lungi; `dir` poate avea nume scurte 8.3
/// (`C:\Users\RUNNER~1\...`) sau legături: se compară și forma canonică.
fn dir_forms(dir: &Path) -> Vec<String> {
    let mut v = vec![norm(dir)];
    if let Ok(c) = std::fs::canonicalize(dir) {
        let c = norm(&c);
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

impl Fs for SysFs {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        StdFs.list_dir(dir)
    }
    fn created(&self, p: &Path) -> io::Result<i64> {
        StdFs.created(p)
    }
    fn stat_full(&self, p: &Path) -> io::Result<(FileStat, i64)> {
        StdFs.stat_full(p)
    }
    fn in_use(&self, p: &Path) -> bool {
        StdFs.in_use(p)
    }

    fn read_prefix(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
        StdFs.read_prefix(p, max)
    }
    /// Pe bucăți, cu oprire la prima diferență (implicitul din trăsătură citea ambele
    /// fișiere întregi în memorie: un film de 4 GB, de două ori).
    fn same_content(&self, a: &Path, b: &Path) -> io::Result<bool> {
        StdFs.same_content(a, b)
    }
    fn hash_file(&self, p: &Path) -> io::Result<u64> {
        StdFs.hash_file(p)
    }

    fn exists(&self, p: &Path) -> bool {
        StdFs.exists(p)
    }
    fn metadata(&self, p: &Path) -> io::Result<FileStat> {
        StdFs.metadata(p)
    }
    fn create_dir_all(&mut self, p: &Path) -> io::Result<()> {
        StdFs.create_dir_all(p)
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        StdFs.rename(from, to)
    }
    fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64> {
        StdFs.copy(from, to)
    }
    fn remove_file(&mut self, p: &Path) -> io::Result<()> {
        StdFs.remove_file(p)
    }
    fn remove_dir_if_empty(&mut self, p: &Path) -> io::Result<bool> {
        StdFs.remove_dir_if_empty(p)
    }
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<i32> {
        StdFs.run(program, args)
    }
    fn run_shell(&mut self, line: &str) -> io::Result<i32> {
        StdFs.run_shell(line)
    }
    fn unpack(&mut self, archive: &Path, into: &Path) -> io::Result<()> {
        StdFs.unpack(archive, into)
    }

    fn trash(&mut self, p: &Path) -> io::Result<()> {
        trash::delete(p).map_err(other)
    }

    /// Readuce din Coș cel mai recent fișier șters de la `original`. Numele se compară și
    /// fără extensie, fiindcă Windows poate ascunde extensiile în Coș.
    fn restore_from_trash(&mut self, original: &Path) -> io::Result<()> {
        let parent = original.parent().ok_or_else(|| other("cale fără folder"))?;
        let full = original.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let stem = original.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let items = trash::os_limited::list().map_err(other)?;
        let forms = dir_forms(parent);
        let found = items
            .into_iter()
            .filter(|i| {
                let n = i.name.to_string_lossy().to_lowercase();
                forms.contains(&norm(&i.original_parent)) && (n == full || n == stem)
            })
            .max_by_key(|i| i.time_deleted)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fișierul nu e în Coșul de reciclare"))?;
        // Cu „Ascunde extensiile tipurilor cunoscute” (implicit în Windows), Coșul dă numele
        // fără extensie și fișierul revine ca `factura` în loc de `factura.pdf`: i se pune
        // înapoi numele original.
        let restored_as = parent.join(&found.name);
        if original.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "există deja un fișier cu acest nume"));
        }
        let can_fix = !restored_as.exists() || restored_as == original;
        trash::os_limited::restore_all([found]).map_err(other)?;
        if !original.exists() && restored_as.exists() && can_fix {
            std::fs::rename(&restored_as, original)?;
        }
        if original.exists() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "restaurat, dar fișierul nu a apărut la locul lui"))
        }
    }
}
