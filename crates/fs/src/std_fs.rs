use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Cât așteaptă daemonul o comandă. Una care durează mai mult continuă în fundal, iar
/// regula merge mai departe (altfel ar ține pe loc toate celelalte fișiere).
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Câți octeți de la sfârșitul ieșirii de eroare se păstrează pentru jurnal.
const ERR_TAIL: usize = 2048;

use crate::{DirEntry, FileStat, Fs};

/// Implementarea reală peste `std::fs`.
///
/// Coșul de reciclare nu e disponibil fără o bibliotecă externă: `trash` și
/// `restore_from_trash` întorc `Unsupported` până când daemonul cu dependențe
/// își înregistrează propria implementare (pe Windows, `IFileOperation`).
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

fn unix_secs(t: std::time::SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

fn stat_of(m: &fs::Metadata) -> (FileStat, i64) {
    let modified = m.modified().map(unix_secs).unwrap_or(0);
    (FileStat { size: m.len(), modified }, m.created().map(unix_secs).unwrap_or(modified))
}

impl Fs for StdFs {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir)? {
            let e = e?;
            let ft = e.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let is_dir = ft.is_dir();
            // Pe Windows `DirEntry::metadata` nu face nicio citire în plus.
            let stat = if is_dir { None } else { e.metadata().ok().map(|m| stat_of(&m)) };
            out.push(DirEntry { path: e.path(), is_dir, stat });
        }
        Ok(out)
    }

    fn created(&self, p: &Path) -> io::Result<i64> {
        let m = fs::metadata(p)?;
        Ok(m.created().or_else(|_| m.modified()).map(unix_secs).unwrap_or(0))
    }

    fn stat_full(&self, p: &Path) -> io::Result<(FileStat, i64)> {
        Ok(stat_of(&fs::metadata(p)?))
    }

    fn in_use(&self, p: &Path) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Fără partajare: orice alt handle deschis pe fișier face deschiderea să eșueze
            // cu ERROR_SHARING_VIOLATION (32) sau ERROR_LOCK_VIOLATION (33).
            match fs::OpenOptions::new().read(true).share_mode(0).open(p) {
                Ok(_) => false,
                Err(e) => matches!(e.raw_os_error(), Some(32 | 33)),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = p;
            false
        }
    }

    fn same_content(&self, a: &Path, b: &Path) -> io::Result<bool> {
        let (fa, fb) = (fs::File::open(a)?, fs::File::open(b)?);
        if fa.metadata()?.len() != fb.metadata()?.len() {
            return Ok(false);
        }
        // Pe bucăți, cu oprire la prima diferență: fișierele mari nu intră în memorie.
        let (mut ra, mut rb) = (io::BufReader::with_capacity(1 << 16, fa), io::BufReader::with_capacity(1 << 16, fb));
        let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
        loop {
            let n = read_full(&mut ra, &mut ba)?;
            let m = read_full(&mut rb, &mut bb)?;
            if n != m || ba[..n] != bb[..m] {
                return Ok(false);
            }
            if n == 0 {
                return Ok(true);
            }
        }
    }

    fn hash_file(&self, p: &Path) -> io::Result<u64> {
        let mut r = io::BufReader::with_capacity(1 << 20, fs::File::open(p)?);
        let mut buf = vec![0u8; 1 << 20];
        let mut h = crate::Hasher64::default();
        loop {
            let n = read_full(&mut r, &mut buf)?;
            if n == 0 {
                return Ok(h.finish());
            }
            h.update(&buf[..n]);
        }
    }

    fn read_prefix(&self, p: &Path, max: u64) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        fs::File::open(p)?.take(max).read_to_end(&mut out)?;
        Ok(out)
    }

    fn exists(&self, p: &Path) -> bool {
        p.symlink_metadata().is_ok()
    }

    fn metadata(&self, p: &Path) -> io::Result<FileStat> {
        let m = fs::metadata(p)?;
        Ok(FileStat { size: m.len(), modified: m.modified().map(unix_secs).unwrap_or(0) })
    }

    fn create_dir_all(&mut self, p: &Path) -> io::Result<()> {
        fs::create_dir_all(p)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn copy(&mut self, from: &Path, to: &Path) -> io::Result<u64> {
        let n = fs::copy(from, to)?;
        // Păstrează data modificării, ca o mutare între volume să nu o schimbe.
        let modified = fs::metadata(from)?.modified()?;
        fs::OpenOptions::new().write(true).open(to)?.set_modified(modified)?;
        Ok(n)
    }

    fn remove_file(&mut self, p: &Path) -> io::Result<()> {
        fs::remove_file(p)
    }

    fn trash(&mut self, _p: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Coșul de reciclare nu e disponibil în StdFs"))
    }

    fn restore_from_trash(&mut self, _original: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Coșul de reciclare nu e disponibil în StdFs"))
    }

    fn remove_dir_if_empty(&mut self, p: &Path) -> io::Result<bool> {
        match fs::remove_dir(p) {
            Ok(()) => Ok(true),
            // Folderul nu e gol (sau nu există): rămâne.
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Rulează programul fără fereastră de consolă. Un cod de ieșire diferit de 0 devine
    /// eroare, cu ultimele rânduri scrise de program la stderr (pentru jurnal).
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<i32> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        run_command(cmd)
    }

    fn run_shell(&mut self, line: &str) -> io::Result<i32> {
        #[cfg(windows)]
        let cmd = {
            use std::os::windows::process::CommandExt;
            // `/S` + ghilimelele exterioare: cmd scoate doar perechea exterioară, linia rămâne exact cum e.
            let mut c = Command::new("cmd.exe");
            c.raw_arg(format!("/D /S /C \"{line}\""));
            c
        };
        #[cfg(not(windows))]
        let cmd = {
            let mut c = Command::new("sh");
            c.arg("-c").arg(line);
            c
        };
        run_command(cmd)
    }
}

/// Pornește comanda fără fereastră și fără intrare; o așteaptă cel mult `RUN_TIMEOUT`.
fn run_command(mut cmd: Command) -> io::Result<i32> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn()?;
    // Ieșirea de eroare se citește pe alt fir, ca programul să nu se blocheze scriind.
    let pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut tail = Vec::new();
        if let Some(mut p) = pipe {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = p.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > ERR_TAIL {
                    tail.drain(..tail.len() - ERR_TAIL);
                }
            }
        }
        tail
    });
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            let code = status.code().unwrap_or(-1);
            if code == 0 {
                return Ok(0);
            }
            let err = reader.join().unwrap_or_default();
            let text = String::from_utf8_lossy(&err);
            let text = text.trim();
            return Err(io::Error::other(if text.is_empty() {
                format!("cod de ieșire {code}")
            } else {
                format!("cod de ieșire {code}: {text}")
            }));
        }
        if Instant::now() >= deadline {
            return Ok(0); // continuă în fundal
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Umple bufferul cât se poate (o citire poate întoarce mai puțin decât s-a cerut).
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}
