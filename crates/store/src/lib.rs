//! Stocarea durabilă în SQLite: jurnalul operațiilor și starea fișierelor procesate.
//!
//! `Store` implementează `Journal`, deci poate înlocui `MemJournal` fără alte schimbări.
//! Jurnalul e „write-ahead”: `begin` scrie intenția și o sincronizează pe disc înainte să
//! înceapă operația pe fișier. Dacă scrierea în jurnal eșuează, programul se oprește
//! (panic) în loc să modifice fișiere fără urmă în jurnal.

use std::fmt;
use std::path::{Path, PathBuf};

use org_engine::{PrintRow, StateChange, VerdictRow};
use org_fs::{FileStat, PRINT_VERSION};
use org_journal::{Entry, Journal, NewEntry, OpKind, Pending, Status};
use rusqlite::{params, Connection, Row};

const SCHEMA_VERSION: i64 = 5;

const SCHEMA_V1: &str = "
CREATE TABLE journal (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     INTEGER NOT NULL,
    rule_id    INTEGER NOT NULL,
    ts         INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    src        TEXT NOT NULL,
    dst        TEXT,
    size       INTEGER NOT NULL,
    mtime      INTEGER NOT NULL,
    status     TEXT NOT NULL,
    status_msg TEXT
);
CREATE INDEX journal_run ON journal(run_id);
CREATE INDEX journal_open ON journal(status) WHERE status = 'in_progress';
CREATE TABLE file_state (
    rule_id INTEGER NOT NULL,
    path    TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    PRIMARY KEY (rule_id, path)
) WITHOUT ROWID;
CREATE INDEX file_state_path ON file_state(path);
";

/// Schema 3: confirmările cerute de „Cere confirmare” (o cerere pe regulă și fișier).
const MIGRATE_V3: &str = "
CREATE TABLE IF NOT EXISTS pending (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_id INTEGER NOT NULL,
    path    TEXT NOT NULL,
    root    TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    step    INTEGER NOT NULL,
    ts      INTEGER NOT NULL,
    UNIQUE (rule_id, path)
);
";

/// Schema 4: catalogul amprentelor pentru dubluri (`org_engine::Prints`). `v` = versiunea
/// algoritmului: rândurile altei versiuni se ignoră și se rescriu.
const MIGRATE_V4: &str = "
CREATE TABLE IF NOT EXISTS prints (
    path     TEXT PRIMARY KEY,
    size     INTEGER NOT NULL,
    modified INTEGER NOT NULL,
    quick    INTEGER,
    full     INTEGER,
    v        INTEGER NOT NULL
) WITHOUT ROWID;
";

/// Schema 5: rezultatele condițiilor pe conținut (`org_engine::Prints::verdict`), pe fișier și
/// cheie de condiție, valabile pentru o versiune a fișierului (dimensiune, dată).
const MIGRATE_V5: &str = "
CREATE TABLE IF NOT EXISTS verdicts (
    path     TEXT NOT NULL,
    key      INTEGER NOT NULL,
    size     INTEGER NOT NULL,
    modified INTEGER NOT NULL,
    matched  INTEGER NOT NULL,
    PRIMARY KEY (path, key)
) WITHOUT ROWID;
";

/// Schema 2:
/// - index pentru restaurări (`status_msg` = unde a revenit fișierul la anulare);
/// - starea fișierelor păstrează doar fișierele chiar procesate. Versiunea 1 memora și
///   fișierele care nu se potriveau, ceea ce bloca regulile editate și condițiile
///   „mai vechi de N zile”. Rămân doar intrările care au o operație în jurnal pentru
///   aceeași regulă și cale (sursă sau destinație).
const MIGRATE_V2: &str = "
CREATE INDEX IF NOT EXISTS journal_undone ON journal(rule_id, status_msg) WHERE status = 'undone';
CREATE INDEX IF NOT EXISTS tmp_journal_src ON journal(rule_id, src);
CREATE INDEX IF NOT EXISTS tmp_journal_dst ON journal(rule_id, dst);
DELETE FROM file_state WHERE NOT EXISTS (
    SELECT 1 FROM journal j WHERE j.rule_id = file_state.rule_id AND j.src = file_state.path
) AND NOT EXISTS (
    SELECT 1 FROM journal j WHERE j.rule_id = file_state.rule_id AND j.dst = file_state.path
);
DROP INDEX IF EXISTS tmp_journal_src;
DROP INDEX IF EXISTS tmp_journal_dst;
";

/// Cât așteaptă o scriere când celălalt proces (daemonul sau fereastra) ține baza ocupată.
/// Fără el, SQLite răspunde imediat „busy”, iar jurnalul oprește procesul.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Filtrele jurnalului din fereastră. Parametrii: ?1 regula, ?2 starea, ?3 de la, ?4 până la,
/// ?5 text (LIKE, cu `\\` ca escape), apoi ?6 limita și ?7 deplasarea.
const JOURNAL_FILTER: &str = "(?1 IS NULL OR rule_id = ?1) AND (?2 IS NULL OR status = ?2)
    AND (?3 IS NULL OR ts >= ?3) AND (?4 IS NULL OR ts < ?4)
    AND (?5 IS NULL OR src LIKE ?5 ESCAPE '\\' OR dst LIKE ?5 ESCAPE '\\' OR status_msg LIKE ?5 ESCAPE '\\')";

#[derive(Debug, Clone, Default)]
pub struct JournalQuery {
    /// Bucată din calea sursă, destinație sau din mesajul de eroare.
    pub text: Option<String>,
    pub rule: Option<u64>,
    /// `done`, `failed`, `undone`, `in_progress`.
    pub status: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: u32,
    pub offset: u32,
    /// Regulile ale căror intrări nu se arată (regulile ascunse, cât fereastra e blocată).
    pub exclude_rules: Vec<u64>,
}

/// Câte fișiere au ajuns în fiecare stare într-o perioadă (pagina Acasă).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeriodStats {
    pub done: u64,
    pub failed: u64,
    pub simulated: u64,
    pub undone: u64,
}

/// Regulile excluse (ascunse), ca filtru SQL. Id-urile sunt numere, deci pot sta direct în text.
fn exclude_sql(rules: &[u64]) -> String {
    if rules.is_empty() {
        return String::new();
    }
    let ids: Vec<String> = rules.iter().map(|r| (*r as i64).to_string()).collect();
    format!(" AND rule_id NOT IN ({})", ids.join(","))
}

#[derive(Debug)]
pub enum StoreError {
    Db(rusqlite::Error),
    /// Baza a fost creată de o versiune mai nouă a aplicației.
    NewerSchema(i64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Db(e) => write!(f, "eroare bază de date: {e}"),
            StoreError::NewerSchema(v) => write!(f, "baza de date e de o versiune mai nouă ({v})"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e)
    }
}

fn kind_name(k: OpKind) -> &'static str {
    match k {
        OpKind::Move => "move",
        OpKind::Copy => "copy",
        OpKind::Rename => "rename",
        OpKind::ChangeExt => "change_ext",
        OpKind::Trash => "trash",
        OpKind::Delete => "delete",
        OpKind::Run => "run",
    }
}

fn kind_from(s: &str) -> Option<OpKind> {
    Some(match s {
        "move" => OpKind::Move,
        "copy" => OpKind::Copy,
        "rename" => OpKind::Rename,
        "change_ext" => OpKind::ChangeExt,
        "trash" => OpKind::Trash,
        "delete" => OpKind::Delete,
        "run" => OpKind::Run,
        _ => return None,
    })
}

fn status_parts(s: &Status) -> (&'static str, Option<&str>) {
    match s {
        Status::InProgress => ("in_progress", None),
        Status::Done => ("done", None),
        Status::Failed(m) => ("failed", Some(m)),
        Status::Undone => ("undone", None),
        Status::Simulated => ("simulated", None),
    }
}

fn conv_err(col: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        rusqlite::types::Type::Text,
        format!("valoare necunoscută: {what}").into(),
    )
}

fn row_to_entry(r: &Row<'_>) -> rusqlite::Result<Entry> {
    let kind: String = r.get(4)?;
    let status: String = r.get(9)?;
    let msg: Option<String> = r.get(10)?;
    let dst: Option<String> = r.get(6)?;
    Ok(Entry {
        id: r.get::<_, i64>(0)? as u64,
        run_id: r.get::<_, i64>(1)? as u64,
        rule_id: r.get::<_, i64>(2)? as u64,
        ts: r.get(3)?,
        kind: kind_from(&kind).ok_or_else(|| conv_err(4, &kind))?,
        src: PathBuf::from(r.get::<_, String>(5)?),
        dst: dst.map(PathBuf::from),
        size: r.get::<_, i64>(7)? as u64,
        mtime: r.get(8)?,
        status: match status.as_str() {
            "in_progress" => Status::InProgress,
            "done" => Status::Done,
            "failed" => Status::Failed(msg.unwrap_or_default()),
            "undone" => Status::Undone,
            "simulated" => Status::Simulated,
            other => return Err(conv_err(9, other)),
        },
    })
}

const ENTRY_COLS: &str = "id, run_id, rule_id, ts, kind, src, dst, size, mtime, status, status_msg";

pub struct Store {
    conn: Connection,
    last_run: u64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store, StoreError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema(v));
        }
        if v < 1 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA_V1} PRAGMA user_version = 1; COMMIT;"))?;
        }
        if v < 2 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {MIGRATE_V2} PRAGMA user_version = 2; COMMIT;"))?;
        }
        if v < 3 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {MIGRATE_V3} PRAGMA user_version = 3; COMMIT;"))?;
        }
        if v < 4 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {MIGRATE_V4} PRAGMA user_version = 4; COMMIT;"))?;
        }
        if v < 5 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {MIGRATE_V5} PRAGMA user_version = 5; COMMIT;"))?;
        }
        let last_run: i64 = conn.query_row("SELECT COALESCE(MAX(run_id), 0) FROM journal", [], |r| r.get(0))?;
        Ok(Store { conn, last_run: last_run as u64 })
    }

    // ---------- jurnal: interogări pentru interfață și daemon ----------

    /// Id-ul ultimei intrări (0 dacă jurnalul e gol).
    pub fn last_id(&self) -> u64 {
        self.conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM journal", [], |r| r.get::<_, i64>(0))
            .map_or(0, |v| v as u64)
    }

    /// Intrările cu id mai mare decât `after`, în ordine.
    pub fn entries_after(&self, after: u64) -> Vec<Entry> {
        self.query_entries(
            &format!("SELECT {ENTRY_COLS} FROM journal WHERE id > ?1 ORDER BY id"),
            params![after as i64],
        )
    }

    /// Cele mai recente `limit` intrări, de la cea mai nouă.
    pub fn recent(&self, limit: u32) -> Vec<Entry> {
        self.query_entries(
            &format!("SELECT {ENTRY_COLS} FROM journal ORDER BY id DESC LIMIT ?1"),
            params![limit],
        )
    }

    /// Căutare în jurnal, de la cea mai nouă intrare. Filtrele lipsă (`None`) nu restrâng.
    /// Întoarce pagina cerută și numărul total de intrări care se potrivesc.
    pub fn search(&self, q: &JournalQuery) -> (Vec<Entry>, u64) {
        let like = q.text.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| {
            let esc = t.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            format!("%{esc}%")
        });
        let rule = q.rule.map(|r| r as i64);
        let status = q.status.as_deref();
        let exclude = exclude_sql(&q.exclude_rules);
        let entries = self.query_entries(
            &format!(
                "SELECT {ENTRY_COLS} FROM journal WHERE {JOURNAL_FILTER}{exclude} ORDER BY id DESC LIMIT ?6 OFFSET ?7"
            ),
            params![rule, status, q.since, q.until, like, q.limit, q.offset],
        );
        let total = self
            .conn
            .prepare_cached(&format!("SELECT COUNT(*) FROM journal WHERE {JOURNAL_FILTER}{exclude}"))
            .and_then(|mut st| st.query_row(params![rule, status, q.since, q.until, like], |r| r.get::<_, i64>(0)))
            .map_or(0, |v| v as u64);
        (entries, total)
    }

    pub fn count(&self) -> u64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM journal", [], |r| r.get::<_, i64>(0))
            .map_or(0, |v| v as u64)
    }

    /// Câte fișiere (rulări) au ajuns în fiecare stare în `[from, to)`. O rulare cu mai multe
    /// acțiuni (mutare + redenumire) contează o dată.
    pub fn period_stats(&self, from: i64, to: i64, exclude_rules: &[u64]) -> PeriodStats {
        let exclude = exclude_sql(exclude_rules);
        let mut out = PeriodStats::default();
        let sql = format!(
            "SELECT status, COUNT(DISTINCT run_id) FROM journal WHERE ts >= ?1 AND ts < ?2{exclude} GROUP BY status"
        );
        let Ok(mut st) = self.conn.prepare_cached(&sql) else { return out };
        let Ok(rows) = st.query_map(params![from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) else {
            return out;
        };
        for (status, n) in rows.filter_map(Result::ok) {
            let n = n.max(0) as u64;
            match status.as_str() {
                "done" => out.done += n,
                "failed" => out.failed += n,
                "simulated" => out.simulated += n,
                "undone" => out.undone += n,
                _ => {}
            }
        }
        out
    }

    /// Regulile care au lucrat cel mai mult de la `since`: (regulă, fișiere reușite).
    pub fn top_rules(&self, since: i64, exclude_rules: &[u64], limit: u32) -> Vec<(u64, u64)> {
        let exclude = exclude_sql(exclude_rules);
        let sql = format!(
            "SELECT rule_id, COUNT(DISTINCT run_id) AS n FROM journal WHERE ts >= ?1 AND status = 'done'{exclude} \
             GROUP BY rule_id ORDER BY n DESC, rule_id LIMIT ?2"
        );
        let Ok(mut st) = self.conn.prepare_cached(&sql) else { return Vec::new() };
        let Ok(rows) = st.query_map(params![since, limit], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)?.max(0) as u64)))
        else {
            return Vec::new();
        };
        let out: Vec<(u64, u64)> = rows.filter_map(Result::ok).collect();
        out
    }

    fn query_entries(&self, sql: &str, p: impl rusqlite::Params) -> Vec<Entry> {
        let mut st = self.conn.prepare_cached(sql).expect("interogare jurnal");
        let rows = st.query_map(p, row_to_entry).expect("citire jurnal");
        let out: Vec<Entry> = rows.filter_map(Result::ok).collect();
        out
    }

    /// Șterge intrările încheiate (reușite, eșuate sau anulate) mai vechi de `cutoff_ts`.
    /// Cele în curs nu se ating niciodată. Întoarce câte au fost șterse.
    pub fn prune_journal(&mut self, cutoff_ts: i64) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            // Rămân: operațiile în curs (recuperare) și restaurările care țin minte unde a
            // revenit un fișier (altfel regula l-ar procesa din nou după curățenie).
            "DELETE FROM journal WHERE ts < ?1 AND status <> 'in_progress'
               AND NOT (status = 'undone' AND status_msg IS NOT NULL)",
            params![cutoff_ts],
        )?)
    }

    // ---------- catalogul amprentelor ----------

    /// Tot catalogul (doar rândurile versiunii curente a algoritmului).
    pub fn load_prints(&self) -> Result<Vec<(PathBuf, PrintRow)>, StoreError> {
        let mut st = self.conn.prepare_cached("SELECT path, size, modified, quick, full FROM prints WHERE v = ?1")?;
        let rows = st.query_map(params![PRINT_VERSION], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                PrintRow {
                    size: r.get::<_, i64>(1)? as u64,
                    modified: r.get(2)?,
                    quick: r.get::<_, Option<i64>>(3)?.map(|x| x as u64),
                    full: r.get::<_, Option<i64>>(4)?.map(|x| x as u64),
                },
            ))
        })?;
        let out: Vec<(PathBuf, PrintRow)> = rows.collect::<Result<_, _>>()?;
        Ok(out)
    }

    pub fn save_prints(&mut self, rows: &[(PathBuf, PrintRow)]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO prints (path, size, modified, quick, full, v) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (p, r) in rows {
                st.execute(params![
                    p.to_string_lossy(),
                    r.size as i64,
                    r.modified,
                    r.quick.map(|x| x as i64),
                    r.full.map(|x| x as i64),
                    PRINT_VERSION
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Șterge tot ce știe catalogul despre aceste fișiere (amprente și rezultate).
    pub fn delete_prints(&mut self, paths: &[PathBuf]) -> Result<(), StoreError> {
        if paths.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut a = tx.prepare_cached("DELETE FROM prints WHERE path = ?1")?;
            let mut b = tx.prepare_cached("DELETE FROM verdicts WHERE path = ?1")?;
            for p in paths {
                a.execute(params![p.to_string_lossy()])?;
                b.execute(params![p.to_string_lossy()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_verdicts(&self) -> Result<Vec<((PathBuf, u64), VerdictRow)>, StoreError> {
        let mut st = self.conn.prepare_cached("SELECT path, key, size, modified, matched FROM verdicts")?;
        let rows = st.query_map([], |r| {
            Ok((
                (PathBuf::from(r.get::<_, String>(0)?), r.get::<_, i64>(1)? as u64),
                VerdictRow { size: r.get::<_, i64>(2)? as u64, modified: r.get(3)?, matched: r.get::<_, i64>(4)? != 0 },
            ))
        })?;
        let out: Vec<((PathBuf, u64), VerdictRow)> = rows.collect::<Result<_, _>>()?;
        Ok(out)
    }

    pub fn save_verdicts(&mut self, rows: &[((PathBuf, u64), VerdictRow)]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO verdicts (path, key, size, modified, matched) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for ((p, key), r) in rows {
                st.execute(params![p.to_string_lossy(), *key as i64, r.size as i64, r.modified, i64::from(r.matched)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Doar pentru teste: simulează o oprire bruscă; intrările închise cu eroarea
    /// injectată de `MemFs` redevin „în curs”.
    pub fn simulate_crash(&mut self) {
        self.conn
            .execute(
                "UPDATE journal SET status = 'in_progress', status_msg = NULL
                 WHERE status = 'failed' AND status_msg LIKE '%injected%'",
                [],
            )
            .expect("simulate_crash");
    }

    // ---------- starea fișierelor ----------

    pub fn load_file_state(&self) -> Result<Vec<(u64, PathBuf, FileStat)>, StoreError> {
        let mut st = self.conn.prepare("SELECT rule_id, path, size, mtime FROM file_state")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                PathBuf::from(r.get::<_, String>(1)?),
                FileStat { size: r.get::<_, i64>(2)? as u64, modified: r.get(3)? },
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Fișierele procesate de o regulă (pentru previzualizare).
    pub fn processed_paths(&self, rule_id: u64) -> Result<Vec<PathBuf>, StoreError> {
        let mut st = self.conn.prepare_cached("SELECT path FROM file_state WHERE rule_id = ?1")?;
        let rows = st.query_map(params![rule_id as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.map(|r| r.map(PathBuf::from)).collect::<Result<Vec<_>, _>>()?)
    }

    /// Aplică într-o tranzacție schimbările înregistrate de `FileState`.
    pub fn save_state_changes(&mut self, changes: &[StateChange]) -> Result<(), StoreError> {
        if changes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for c in changes {
            match c {
                StateChange::Set(rule, path, stat) => {
                    tx.execute(
                        "INSERT OR REPLACE INTO file_state (rule_id, path, size, mtime) VALUES (?1, ?2, ?3, ?4)",
                        params![*rule as i64, path.to_string_lossy(), stat.size as i64, stat.modified],
                    )?;
                }
                StateChange::ClearPath(path) => {
                    tx.execute("DELETE FROM file_state WHERE path = ?1", params![path.to_string_lossy()])?;
                }
                StateChange::Clear(rule, path) => {
                    tx.execute(
                        "DELETE FROM file_state WHERE rule_id = ?1 AND path = ?2",
                        params![*rule as i64, path.to_string_lossy()],
                    )?;
                }
                StateChange::ClearRule(rule) => {
                    tx.execute("DELETE FROM file_state WHERE rule_id = ?1", params![*rule as i64])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Șterge din stare fișierele care nu mai există (cele mutate de reguli).
    pub fn forget_missing(&mut self, exists: impl Fn(&Path) -> bool) -> Result<usize, StoreError> {
        let paths: Vec<String> = {
            let mut st = self.conn.prepare("SELECT DISTINCT path FROM file_state")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let gone: Vec<&String> = paths.iter().filter(|p| !exists(Path::new(p))).collect();
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for p in &gone {
            n += tx.execute("DELETE FROM file_state WHERE path = ?1", params![p])?;
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn file_state_count(&self) -> u64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM file_state", [], |r| r.get::<_, i64>(0))
            .map_or(0, |v| v as u64)
    }
}

impl Journal for Store {
    fn next_run_id(&mut self) -> u64 {
        self.last_run += 1;
        self.last_run
    }

    fn begin(&mut self, e: NewEntry) -> u64 {
        self.conn
            .execute(
                "INSERT INTO journal (run_id, rule_id, ts, kind, src, dst, size, mtime, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'in_progress')",
                params![
                    e.run_id as i64,
                    e.rule_id as i64,
                    e.ts,
                    kind_name(e.kind),
                    e.src.to_string_lossy(),
                    e.dst.as_ref().map(|d| d.to_string_lossy().into_owned()),
                    e.size as i64,
                    e.mtime
                ],
            )
            .expect("nu s-a putut scrie în jurnal; mă opresc înainte să ating fișiere");
        self.conn.last_insert_rowid() as u64
    }

    fn finish(&mut self, id: u64, status: Status) {
        let (name, msg) = status_parts(&status);
        self.conn
            .execute(
                "UPDATE journal SET status = ?1, status_msg = ?2 WHERE id = ?3",
                params![name, msg, id as i64],
            )
            .expect("nu s-a putut închide intrarea din jurnal");
    }

    fn get(&self, id: u64) -> Option<Entry> {
        self.query_entries(&format!("SELECT {ENTRY_COLS} FROM journal WHERE id = ?1"), params![id as i64])
            .into_iter()
            .next()
    }

    fn in_progress(&self) -> Vec<Entry> {
        self.query_entries(
            &format!("SELECT {ENTRY_COLS} FROM journal WHERE status = 'in_progress' ORDER BY id"),
            [],
        )
    }

    fn by_run(&self, run_id: u64) -> Vec<Entry> {
        self.query_entries(
            &format!("SELECT {ENTRY_COLS} FROM journal WHERE run_id = ?1 ORDER BY id"),
            params![run_id as i64],
        )
    }

    fn mark_undone(&mut self, id: u64, restored: &Path) {
        self.conn
            .execute(
                "UPDATE journal SET status = 'undone', status_msg = ?1 WHERE id = ?2",
                params![restored.to_string_lossy(), id as i64],
            )
            .expect("nu s-a putut închide intrarea din jurnal");
    }

    fn forget_restored(&mut self, rule_id: u64, path: Option<&Path>) {
        // Intrarea rămâne „anulată” în jurnal; dispare doar legătura cu locul restaurării.
        let p = path.map(|p| p.to_string_lossy().into_owned());
        self.conn
            .execute(
                "UPDATE journal SET status_msg = NULL
                 WHERE status = 'undone' AND rule_id = ?1 AND (?2 IS NULL OR status_msg = ?2)",
                params![rule_id as i64, p],
            )
            .expect("nu s-a putut actualiza jurnalul");
    }

    fn add_pending(&mut self, p: Pending) {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO pending (rule_id, path, root, size, mtime, step, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    p.rule_id as i64,
                    p.path.to_string_lossy(),
                    p.root.to_string_lossy(),
                    p.size as i64,
                    p.mtime,
                    p.step as i64,
                    p.ts
                ],
            )
            .expect("nu s-a putut scrie confirmarea");
    }

    fn pending(&self) -> Vec<Pending> {
        let Ok(mut st) = self
            .conn
            .prepare_cached("SELECT id, rule_id, path, root, size, mtime, step, ts FROM pending ORDER BY id DESC")
        else {
            return Vec::new();
        };
        st.query_map([], |r| {
            Ok(Pending {
                id: r.get::<_, i64>(0)? as u64,
                rule_id: r.get::<_, i64>(1)? as u64,
                path: PathBuf::from(r.get::<_, String>(2)?),
                root: PathBuf::from(r.get::<_, String>(3)?),
                size: r.get::<_, i64>(4)? as u64,
                mtime: r.get(5)?,
                step: r.get::<_, i64>(6)? as usize,
                ts: r.get(7)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    fn remove_pending(&mut self, id: u64) {
        let _ = self.conn.execute("DELETE FROM pending WHERE id = ?1", params![id as i64]);
    }

    fn restored_to(&self, rule_id: u64, path: &Path, size: u64, mtime: i64) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM journal WHERE status = 'undone' AND rule_id = ?1
                     AND status_msg = ?2 AND size = ?3 AND mtime = ?4)",
                params![rule_id as i64, path.to_string_lossy(), size as i64, mtime],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }
}
