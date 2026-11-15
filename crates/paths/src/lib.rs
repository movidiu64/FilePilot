//! Where the program keeps its data, and the lock that keeps a single daemon running.
//!
//! The data folder is `%APPDATA%\FilePilot` on Windows (moved automatically from `Organizator`,
//! the former name). The `FILEPILOT_DATA` variable overrides it (tests, portable setups).
//! The database keeps its historical name, `organizator.db`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

pub const RULES_FILE: &str = "rules.orgrules";
pub const DB_FILE: &str = "organizator.db";
pub const LOCK_FILE: &str = "daemon.lock";
/// Preferințele comune ferestrei și daemonului (deocamdată limba).
pub const SETTINGS_FILE: &str = "settings.json";
/// Starea daemonului pentru fereastră (reguli încărcate, foldere, pauză, numărători).
pub const STATUS_FILE: &str = "daemon-status.json";
/// Pauza: momentul (secunde Unix) până la care daemonul nu procesează; `0` = până la reluare.
pub const PAUSE_FILE: &str = "pause";

/// Calea unei cereri noi către daemon. Se scrie întâi sub numele `.tmp` și se redenumește,
/// ca daemonul să nu citească niciodată o cerere pe jumătate scrisă.
pub fn new_request_path(dir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    dir.join(format!("request-{t}-{}-{}.json", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Este un fișier de cerere complet (nu unul temporar)?
pub fn is_request_file(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("request-") && n.ends_with(".json"))
}

/// Scrie o cerere atomic (temporar + redenumire).
pub fn write_request(dir: &Path, text: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = new_request_path(dir);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Cererile rămase în folder (de exemplu scrise cât daemonul era oprit), în ordinea sosirii.
pub fn pending_requests(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_request_file(p)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Copiile regulilor, în folderul de date: `backups\rules-AAAA-LL-ZZ_HH-MM-SS.orgrules`.
pub const BACKUP_DIR: &str = "backups";

/// Copiile existente ale regulilor, de la cea mai veche (numele se sortează după dată).
pub fn rule_backups(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir.join(BACKUP_DIR))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rules-") && n.ends_with(".orgrules"))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Înainte de a suprascrie regulile: copiază fișierul actual în `backups` (`stamp` = data
/// locală, sortabilă). Nu face copie dacă ultima are mai puțin de `min_gap` (o sesiune de
/// editare lasă o singură copie, cu starea de dinainte) și păstrează cel mult `keep` copii.
pub fn backup_rules(
    dir: &Path,
    rules: &Path,
    stamp: &str,
    min_gap: std::time::Duration,
    keep: usize,
) -> io::Result<Option<PathBuf>> {
    if !rules.is_file() {
        return Ok(None);
    }
    let existing = rule_backups(dir);
    if let Some(last) = existing.last() {
        let age = fs::metadata(last).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a < min_gap) {
            return Ok(None);
        }
    }
    let bdir = dir.join(BACKUP_DIR);
    fs::create_dir_all(&bdir)?;
    let mut target = bdir.join(format!("rules-{stamp}.orgrules"));
    let mut n = 2;
    while target.exists() {
        target = bdir.join(format!("rules-{stamp}_{n}.orgrules"));
        n += 1;
    }
    fs::copy(rules, &target)?;
    let all = rule_backups(dir);
    for old in all.iter().take(all.len().saturating_sub(keep.max(1))) {
        let _ = fs::remove_file(old);
    }
    Ok(Some(target))
}

/// The app's data folder (not created). `FILEPILOT_DATA` overrides it (tests, portable
/// setups); `ORGANIZATOR_DATA`, from the app's former name, is still honoured.
pub fn data_dir() -> PathBuf {
    for var in ["FILEPILOT_DATA", "ORGANIZATOR_DATA"] {
        if let Some(d) = std::env::var_os(var) {
            return PathBuf::from(d);
        }
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        return migrate_legacy_dir(Path::new(&a));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(x).join("filepilot");
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".local/share/filepilot"),
        None => PathBuf::from("filepilot-data"),
    }
}

/// `base\FilePilot`. Before the rename the folder was `base\Organizator`: it is moved once,
/// keeping the rules, journal and catalog. If it can't be moved (the old daemon still has it
/// open), the old folder keeps being used until the next start.
pub fn migrate_legacy_dir(base: &Path) -> PathBuf {
    let new = base.join("FilePilot");
    let old = base.join("Organizator");
    if new.exists() || !old.exists() {
        return new;
    }
    match fs::rename(&old, &new) {
        Ok(()) => new,
        // Another process (window or daemon) may have just moved it.
        Err(_) if new.exists() => new,
        Err(_) => old,
    }
}

#[derive(Debug, Clone)]
pub struct DataFiles {
    pub dir: PathBuf,
    pub rules: PathBuf,
    pub db: PathBuf,
    pub lock: PathBuf,
    pub settings: PathBuf,
}

impl DataFiles {
    pub fn in_dir(dir: &Path) -> DataFiles {
        DataFiles {
            dir: dir.to_path_buf(),
            rules: dir.join(RULES_FILE),
            db: dir.join(DB_FILE),
            lock: dir.join(LOCK_FILE),
            settings: dir.join(SETTINGS_FILE),
        }
    }

    /// Fișierele din folderul implicit; creează folderul dacă lipsește.
    pub fn default_ensured() -> io::Result<DataFiles> {
        let f = DataFiles::in_dir(&data_dir());
        fs::create_dir_all(&f.dir)?;
        Ok(f)
    }
}

/// Lacătul deținut cât timp daemonul rulează. Se eliberează la oprire, chiar și brusc,
/// fiindcă îl ține sistemul de operare.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

/// Încearcă să ia lacătul. `Ok(None)` = alt daemon îl ține deja.
pub fn acquire_lock(path: &Path) -> io::Result<Option<InstanceLock>> {
    let f = open_lock_file(path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: f })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Rulează un daemon acum? (Nu păstrează lacătul.)
pub fn daemon_running(path: &Path) -> bool {
    match open_lock_file(path) {
        Ok(f) => matches!(f.try_lock(), Err(TryLockError::WouldBlock)),
        Err(_) => false,
    }
}
