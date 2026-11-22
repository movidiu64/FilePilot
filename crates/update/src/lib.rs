//! Updates from FilePilot's GitHub releases.
//!
//! Only with the user's consent (the `updates` setting), the daemon asks GitHub once a day for
//! the latest release and notes it in `update.json`; the window shows it and, when asked,
//! downloads the installer, checks it and starts it. The request carries nothing but FilePilot's
//! version in the User-Agent header, and goes through Windows' own `curl.exe`.
//!
//! An installer is run only if both hold:
//! - its SHA-256 is the one GitHub lists for that release file, and its size matches;
//! - it carries a signature made by FilePilot's release build with the key whose public half
//!   is [`PUBLIC_KEY`] (minisign format: Ed25519 over the file's BLAKE2b-512 hash), and the
//!   signed comment names that same file and version, so an older signed installer cannot be
//!   passed off as a newer one.
//!
//! Release files are taken only from this repository's release URLs, over HTTPS.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use org_core::secret::{sha256, to_hex};
use org_portable::json::{self, Value};

/// The public half of the key the release build signs installers with
/// (`tools/sign-release.py public`; the secret half is the FILEPILOT_SIGNING_KEY secret).
pub const PUBLIC_KEY: &str = "RWRewtTei3LE9CeNOYDo5sGnPSA8e8JFCDk2Qxa2LvqhLh4/++YyZ13Y";

/// Where the latest release is described.
pub const LATEST_URL: &str = "https://api.github.com/repos/movidiu64/FilePilot/releases/latest";
const RELEASES: &str = "https://github.com/movidiu64/FilePilot/releases";

/// The file in the data folder that holds what the last check found.
pub const STATE_FILE: &str = "update.json";
/// How long after a check the next one is due, and after one that failed.
pub const CHECK_EVERY: i64 = 24 * 3600;
pub const RETRY_AFTER: i64 = 3 * 3600;
/// The release notes kept for the window, at most (characters).
const MAX_NOTES: usize = 6000;

/// A release version: `major.minor.patch`, compared as numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// "1.4.0" or "v1.4.0"; anything else (a pre-release suffix, leading zeros) is refused.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let num = |p: &str| -> Option<u32> {
            let ok = !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'));
            if ok {
                p.parse().ok()
            } else {
                None
            }
        };
        let mut it = s.split('.');
        let v = Version { major: num(it.next()?)?, minor: num(it.next()?)?, patch: num(it.next()?)? };
        it.next().is_none().then_some(v)
    }

    /// The version of this build.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the package version is major.minor.patch")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The installer of a version, and its signature.
pub fn setup_name(v: Version) -> String {
    format!("FilePilot_{v}_x64-setup.exe")
}

pub fn signature_name(v: Version) -> String {
    format!("{}.minisig", setup_name(v))
}

/// The release page of a version.
pub fn page_url(v: Version) -> String {
    format!("{RELEASES}/tag/v{v}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Lowercase hex, from GitHub's `digest` ("sha256:…").
    pub sha256: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    pub setup: Asset,
    pub signature: Asset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// GitHub could not be reached, or answered with an error.
    Network(String),
    /// Windows' curl.exe is missing (Windows 10 before version 1803).
    NoCurl,
    /// The release is not what FilePilot publishes (no installer, no signature, other URLs).
    BadRelease(String),
    /// The release is not newer than this build.
    NotNewer,
    /// The downloaded installer is not the file GitHub lists (size or SHA-256).
    Hash,
    /// The signature is missing, not made with FilePilot's key, or for another file.
    Signature(String),
    Io(String),
}

impl Error {
    /// A short code the window translates.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Network(_) => "network",
            Error::NoCurl => "no_curl",
            Error::BadRelease(_) => "bad_release",
            Error::NotNewer => "not_newer",
            Error::Hash => "hash",
            Error::Signature(_) => "signature",
            Error::Io(_) => "io",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Error::Network(s) | Error::BadRelease(s) | Error::Signature(s) | Error::Io(s) => s.clone(),
            _ => String::new(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.detail();
        if d.is_empty() {
            f.write_str(self.code())
        } else {
            write!(f, "{}: {d}", self.code())
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

fn get<'a>(o: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    o.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn get_str<'a>(o: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    match get(o, key) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// Reads GitHub's answer for the latest release (`GET /repos/{repo}/releases/latest`).
pub fn parse_latest(text: &str) -> Result<Release, Error> {
    let bad = |s: &str| Error::BadRelease(s.to_string());
    let Ok(Value::Obj(o)) = json::parse(text) else { return Err(bad("not a release")) };
    if matches!(get(&o, "draft"), Some(Value::Bool(true))) || matches!(get(&o, "prerelease"), Some(Value::Bool(true))) {
        return Err(bad("a draft or a pre-release"));
    }
    let tag = get_str(&o, "tag_name").ok_or_else(|| bad("no tag"))?;
    let version = Version::parse(tag).ok_or_else(|| bad("the tag is not a version"))?;
    let mut notes: String = get_str(&o, "body").unwrap_or_default().replace("\r\n", "\n");
    if notes.chars().count() > MAX_NOTES {
        notes = notes.chars().take(MAX_NOTES).collect::<String>() + "…";
    }
    let Some(Value::Arr(assets)) = get(&o, "assets") else { return Err(bad("no files")) };
    let asset = |name: &str| -> Result<Asset, Error> {
        let a = assets
            .iter()
            .filter_map(|a| if let Value::Obj(a) = a { Some(a) } else { None })
            .find(|a| get_str(a, "name") == Some(name))
            .ok_or_else(|| bad(&format!("{name} is missing")))?;
        // Only this repository's release files, for this very version.
        let url = format!("{RELEASES}/download/v{version}/{name}");
        if get_str(a, "browser_download_url") != Some(url.as_str()) {
            return Err(bad(&format!("{name} comes from elsewhere")));
        }
        let sha256 = get_str(a, "digest")
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase)
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        let size = match get(a, "size") {
            Some(Value::Int(n)) if *n > 0 => *n as u64,
            _ => return Err(bad(&format!("{name} has no size"))),
        };
        Ok(Asset { name: name.to_string(), url, sha256, size })
    };
    let setup = asset(&setup_name(version))?;
    if setup.sha256.is_none() {
        return Err(bad("GitHub lists no SHA-256 for the installer"));
    }
    let signature = asset(&signature_name(version))?;
    Ok(Release { version, notes, setup, signature })
}

/// Checks a downloaded installer: size and SHA-256 as GitHub lists them, and FilePilot's
/// signature for that file and version.
pub fn verify_installer(data: &[u8], signature: &str, r: &Release) -> Result<(), Error> {
    let want = r.setup.sha256.as_deref().ok_or(Error::Hash)?;
    if data.len() as u64 != r.setup.size || to_hex(&sha256(&[data])) != want {
        return Err(Error::Hash);
    }
    verify_signature(data, signature, &r.setup.name, r.version, PUBLIC_KEY)
}

/// Checks a minisign signature made with `public_key` (base64, as `minisign -R` prints it)
/// whose signed comment names `name` and `version`.
pub fn verify_signature(data: &[u8], signature: &str, name: &str, version: Version, public_key: &str) -> Result<(), Error> {
    let pk = minisign_verify::PublicKey::from_base64(public_key).map_err(|e| Error::Signature(format!("public key: {e}")))?;
    // A signature file saved with Windows line ends (or stray CRs) reads the same.
    let signature = signature.replace('\r', "");
    let sig = minisign_verify::Signature::decode(&signature).map_err(|e| Error::Signature(e.to_string()))?;
    // Only prehashed signatures (allow_legacy = false), as minisign makes by default.
    pk.verify(data, &sig, false).map_err(|e| Error::Signature(e.to_string()))?;
    let comment = sig.trusted_comment();
    let says = |key: &str, val: &str| comment.split('\t').any(|p| p.strip_prefix(key).and_then(|p| p.strip_prefix(':')) == Some(val));
    if !says("file", name) || !says("version", &version.to_string()) {
        return Err(Error::Signature(format!("signed for something else ({comment})")));
    }
    Ok(())
}

/// What the last check found (`update.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// When GitHub was last asked (seconds since 1970).
    pub checked_at: i64,
    /// The latest release then, and its notes.
    pub latest: Option<Version>,
    pub notes: String,
    /// The error code of that check, if it failed.
    pub error: Option<String>,
}

impl State {
    /// A release newer than this build, if the last check found one.
    pub fn available(&self) -> Option<Version> {
        self.latest.filter(|v| *v > Version::current())
    }

    /// Whether the daemon should ask again (a clock set back counts as due).
    pub fn due(&self, now: i64) -> bool {
        let wait = if self.error.is_some() { RETRY_AFTER } else { CHECK_EVERY };
        now < self.checked_at || now - self.checked_at >= wait
    }

    pub fn to_json(&self) -> String {
        let s = |x: &str| Value::Str(x.to_string());
        json::to_string_pretty(&Value::Obj(vec![
            ("checked_at".into(), Value::Int(self.checked_at)),
            ("latest".into(), self.latest.map_or(Value::Null, |v| s(&v.to_string()))),
            ("notes".into(), s(&self.notes)),
            ("error".into(), self.error.as_deref().map_or(Value::Null, s)),
        ]))
    }

    pub fn from_json(text: &str) -> Self {
        let Ok(Value::Obj(o)) = json::parse(text) else { return Self::default() };
        State {
            checked_at: match get(&o, "checked_at") {
                Some(Value::Int(n)) => *n,
                _ => 0,
            },
            latest: get_str(&o, "latest").and_then(Version::parse),
            notes: get_str(&o, "notes").unwrap_or_default().to_string(),
            error: get_str(&o, "error").map(str::to_string),
        }
    }
}

pub fn read_state(path: &Path) -> State {
    fs::read_to_string(path).map(|t| State::from_json(&t)).unwrap_or_default()
}

pub fn write_state(path: &Path, st: &State) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, st.to_json())?;
    fs::rename(&tmp, path)
}

/// Asks GitHub for the latest release.
pub fn check() -> Result<Release, Error> {
    let body = fetch(LATEST_URL, None, 30)?;
    parse_latest(&String::from_utf8_lossy(&body))
}

/// Asks GitHub and writes down what it found in `path` (also when it failed).
pub fn check_and_record(path: &Path, now: i64) -> State {
    let old = read_state(path);
    let st = match check() {
        Ok(r) => State { checked_at: now, latest: Some(r.version), notes: r.notes, error: None },
        // A failed check keeps what the previous one found.
        Err(e) => State { checked_at: now, error: Some(e.code().to_string()), ..old },
    };
    let _ = write_state(path, &st);
    st
}

/// Downloads the installer of `r` and its signature into `dir` (emptied first) and checks
/// them; returns the installer's path.
pub fn download(r: &Release, dir: &Path) -> Result<PathBuf, Error> {
    if r.version <= Version::current() {
        return Err(Error::NotNewer);
    }
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let setup = dir.join(&r.setup.name);
    let sig = dir.join(&r.signature.name);
    fetch(&r.signature.url, Some(&sig), 60)?;
    fetch(&r.setup.url, Some(&setup), 900)?;
    let data = fs::read(&setup)?;
    let signature = fs::read_to_string(&sig).map_err(|_| Error::Signature("unreadable".into()))?;
    if let Err(e) = verify_installer(&data, &signature, r) {
        // A file that failed the check is not left lying around.
        let _ = fs::remove_dir_all(dir);
        return Err(e);
    }
    Ok(setup)
}

/// Starts the installer: progress only, no questions, as an update; it closes FilePilot,
/// stops and restarts the daemon and opens the window again when done.
pub fn run_installer(setup: &Path) -> Result<(), Error> {
    std::process::Command::new(setup).args(["/P", "/R", "/UPDATE"]).spawn().map(|_| ()).map_err(Error::from)
}

/// GETs `url` over HTTPS with Windows' curl.exe: into `dest`, or returned when `dest` is None.
pub fn fetch(url: &str, dest: Option<&Path>, timeout_secs: u32) -> Result<Vec<u8>, Error> {
    if !url.starts_with("https://") {
        return Err(Error::BadRelease("not https".into()));
    }
    let mut cmd = std::process::Command::new(curl());
    cmd.args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2"])
        .args(["--max-time", &timeout_secs.to_string(), "--max-filesize", "209715200"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", &format!("User-Agent: FilePilot/{}", Version::current())]);
    if let Some(d) = dest {
        cmd.arg("--output").arg(d);
    }
    cmd.arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up from the window or the tray.
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| if e.kind() == io::ErrorKind::NotFound { Error::NoCurl } else { Error::Io(e.to_string()) })?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Err(Error::Network(if msg.is_empty() { format!("curl exited with {}", out.status) } else { msg.to_string() }));
    }
    Ok(out.stdout)
}

/// Windows' own curl.exe (not one found first on PATH).
fn curl() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        root.join("System32").join("curl.exe")
    } else {
        PathBuf::from("curl")
    }
}
