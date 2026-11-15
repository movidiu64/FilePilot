use std::path::{Path, PathBuf};

/// Metadatele unui fișier, așa cum le primește motorul de reguli.
///
/// `root` este folderul monitorizat prin care a fost găsit fișierul; se folosește
/// la păstrarea structurii de subfoldere. Timpii sunt secunde Unix (UTC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub path: PathBuf,
    pub root: PathBuf,
    pub size: u64,
    pub created: i64,
    pub modified: i64,
}

impl FileMeta {
    pub fn new(
        path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        size: u64,
        created: i64,
        modified: i64,
    ) -> Self {
        FileMeta { path: path.into(), root: root.into(), size, created, modified }
    }

    /// Numele fără extensie: `letter` pentru `c:\docs\letter.docx`.
    pub fn stem(&self) -> String {
        self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Extensia fără punct, cu majusculele originale (`docx`). Goală dacă lipsește.
    pub fn extension(&self) -> String {
        self.path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Numele cu extensie.
    pub fn file_name(&self) -> String {
        self.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Calea folderului care conține fișierul.
    pub fn folder(&self) -> String {
        self.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Calea completă.
    pub fn full_path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// Calea folderului față de folderul monitorizat, dacă fișierul e în interiorul lui.
    pub fn relative_parent(&self) -> Option<PathBuf> {
        let parent: &Path = self.path.parent()?;
        parent.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }
}
