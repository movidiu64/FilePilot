//! Search by name, extension, size and date in the watched folders: a small "Everything"
//! for the user's own folders. The folders are listed once into an in-memory index (the
//! listing gives size and dates without opening files); every search after that only
//! filters the index.

use std::path::{Path, PathBuf};

use org_core::WatchedFolder;
use org_fs::Fs;

/// Never index more than this many files (about 150 MB of memory at the very most).
pub const MAX_INDEXED: usize = 1_000_000;

/// One indexed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: PathBuf,
    /// The file name in lowercase, for matching.
    name_lower: String,
    pub size: u64,
    pub modified: i64,
}

impl IndexEntry {
    pub fn name(&self) -> &str {
        self.path.file_name().and_then(|n| n.to_str()).unwrap_or("")
    }
    fn ext_lower(&self) -> &str {
        match self.name_lower.rfind('.') {
            Some(i) if i > 0 => &self.name_lower[i + 1..],
            _ => "",
        }
    }
}

/// Progress of building the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexProgress {
    pub folders: usize,
    pub files: usize,
}

/// The files of a set of folders.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub entries: Vec<IndexEntry>,
    /// The folders indexed, as asked for.
    pub folders: Vec<WatchedFolder>,
    /// Folders that could not be listed (missing drive, no access).
    pub unavailable: Vec<PathBuf>,
    /// The listing stopped at [`MAX_INDEXED`] files.
    pub capped: bool,
}

impl FileIndex {
    /// Lists the folders. `step` is told the progress now and then and returns `false` to
    /// stop; then `None` is returned.
    pub fn build(fs: &dyn Fs, folders: &[WatchedFolder], step: &mut dyn FnMut(IndexProgress) -> bool) -> Option<FileIndex> {
        let mut idx = FileIndex { folders: folders.to_vec(), ..FileIndex::default() };
        let mut seen_roots: Vec<&Path> = Vec::new();
        let mut done_dirs = 0usize;
        'roots: for f in folders {
            // A folder inside another one already listed with its subfolders adds nothing.
            if seen_roots.iter().any(|r| f.path.starts_with(r)) {
                continue;
            }
            if f.recursive {
                seen_roots.push(&f.path);
            }
            let mut stack = vec![(f.path.clone(), true)];
            while let Some((dir, top)) = stack.pop() {
                let list = match fs.list_dir(&dir) {
                    Ok(l) => l,
                    Err(_) => {
                        if top {
                            idx.unavailable.push(dir);
                        }
                        continue;
                    }
                };
                for e in list {
                    if e.is_dir {
                        if f.recursive {
                            stack.push((e.path, false));
                        }
                        continue;
                    }
                    if crate::is_temp_download(&e.path) {
                        continue;
                    }
                    let (size, modified) = match e.stat {
                        Some((st, _)) => (st.size, st.modified),
                        None => match fs.metadata(&e.path) {
                            Ok(st) => (st.size, st.modified),
                            Err(_) => continue,
                        },
                    };
                    let name_lower = e.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    idx.entries.push(IndexEntry { path: e.path, name_lower, size, modified });
                    if idx.entries.len() >= MAX_INDEXED {
                        idx.capped = true;
                        break 'roots;
                    }
                }
                done_dirs += 1;
                if done_dirs.is_multiple_of(64) && !step(IndexProgress { folders: done_dirs, files: idx.entries.len() }) {
                    return None;
                }
            }
        }
        // The same file reached twice (overlapping folders without subfolders): once.
        idx.entries.sort_by(|a, b| a.path.cmp(&b.path));
        idx.entries.dedup_by(|a, b| a.path == b.path);
        step(IndexProgress { folders: done_dirs, files: idx.entries.len() });
        Some(idx)
    }

    /// Whether `path` lies in one of the indexed folders (directly, or below it when the
    /// folder is listed with its subfolders).
    pub fn covers(&self, path: &Path) -> bool {
        self.folders
            .iter()
            .any(|f| if f.recursive { path.starts_with(&f.path) && path != f.path } else { path.parent() == Some(f.path.as_path()) })
    }

    /// Brings the index up to date for files that changed since it was built (moved,
    /// renamed, copied, deleted, restored): each path is looked up again and added, updated
    /// or dropped. Much cheaper than listing the folders again.
    pub fn update(&mut self, fs: &dyn Fs, touched: &[PathBuf]) {
        if touched.is_empty() {
            return;
        }
        let set: std::collections::HashSet<&Path> = touched.iter().map(PathBuf::as_path).collect();
        self.entries.retain(|e| !set.contains(e.path.as_path()));
        for p in set {
            if !self.covers(p) || crate::is_temp_download(p) || self.entries.len() >= MAX_INDEXED {
                continue;
            }
            let Ok(st) = fs.metadata(p) else { continue };
            if fs.list_dir(p).is_ok() {
                continue; // a folder, not a file
            }
            let name_lower = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let at = self.entries.partition_point(|e| e.path.as_path() < p);
            self.entries.insert(at, IndexEntry { path: p.to_path_buf(), name_lower, size: st.size, modified: st.modified });
        }
    }

    /// Drops one file from the index (it turned out to be gone).
    pub fn forget(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
    }

    /// The matching files, sorted, at most `limit`; and how many match in all.
    pub fn search(&self, q: &Query, sort: SortBy, desc: bool, limit: usize) -> (Vec<&IndexEntry>, usize) {
        let words = q.words();
        let exts: Vec<String> = q
            .extensions
            .split([',', ';', ' '])
            .map(|e| e.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        let mut hits: Vec<&IndexEntry> = self
            .entries
            .iter()
            .filter(|e| {
                words.iter().all(|w| w.matches(&e.name_lower))
                    && (exts.is_empty() || exts.iter().any(|x| x == e.ext_lower()))
                    && q.min_size.is_none_or(|m| e.size >= m)
                    && q.max_size.is_none_or(|m| e.size <= m)
                    && q.modified_after.is_none_or(|t| e.modified >= t)
                    && q.modified_before.is_none_or(|t| e.modified < t)
            })
            .collect();
        let total = hits.len();
        let key = |a: &&IndexEntry, b: &&IndexEntry| match sort {
            SortBy::Name => a.name_lower.cmp(&b.name_lower).then_with(|| a.path.cmp(&b.path)),
            SortBy::Folder => a.path.parent().cmp(&b.path.parent()).then_with(|| a.name_lower.cmp(&b.name_lower)),
            SortBy::Size => a.size.cmp(&b.size).then_with(|| a.name_lower.cmp(&b.name_lower)),
            SortBy::Modified => a.modified.cmp(&b.modified).then_with(|| a.name_lower.cmp(&b.name_lower)),
        };
        // Only the shown part needs to be fully sorted.
        if hits.len() > limit && limit > 0 {
            let cmp = |a: &&IndexEntry, b: &&IndexEntry| if desc { key(b, a) } else { key(a, b) };
            hits.select_nth_unstable_by(limit - 1, cmp);
            hits.truncate(limit);
            hits.sort_by(cmp);
        } else {
            hits.sort_by(|a, b| if desc { key(b, a) } else { key(a, b) });
            hits.truncate(limit);
        }
        (hits, total)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Folder,
    Size,
    Modified,
}

/// What to look for. Empty fields do not filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Words that must all appear in the file name, in any order. A word with `*` or `?`
    /// is a wildcard for the whole name (`*.pdf`, `invoice-??.docx`).
    pub text: String,
    /// `pdf, docx`
    pub extensions: String,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
}

enum Word {
    Part(String),
    Wild(Vec<char>),
}

impl Word {
    fn matches(&self, name: &str) -> bool {
        match self {
            Word::Part(p) => name.contains(p.as_str()),
            Word::Wild(p) => {
                let n: Vec<char> = name.chars().collect();
                wildcard(p, &n)
            }
        }
    }
}

impl Query {
    fn words(&self) -> Vec<Word> {
        self.text
            .split_whitespace()
            .map(|w| {
                let w = w.to_lowercase();
                if w.contains(['*', '?']) { Word::Wild(w.chars().collect()) } else { Word::Part(w) }
            })
            .collect()
    }
}

/// `*` any run of characters, `?` one character; linear-time matching with backtracking
/// only to the last `*`.
fn wildcard(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}
