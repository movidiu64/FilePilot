use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::condition::TextField;
use crate::meta::FileMeta;
use crate::timefmt::{self, Lang};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSrc {
    Created,
    Modified,
    Now,
}

/// O variabilă dintr-un șablon.
#[derive(Debug, Clone, PartialEq)]
pub enum Var {
    /// Numele fără extensie: `{name}`.
    Name,
    /// Extensia fără punct: `{ext}`.
    Ext,
    /// Numele cu extensie: `{file}`.
    FileName,
    /// Folderul care conține fișierul: `{folder}`.
    Folder,
    /// Calea completă: `{path}`.
    Path,
    /// Dimensiunea în octeți: `{size}`.
    Size,
    /// O dată formatată: `{created:AAAA-LL-ZZ}`, `{modified:...}`, `{now:...}`.
    Date { src: DateSrc, fmt: String },
    /// Un dicționar definit de utilizator: `{var:Furnizor}`.
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Lit(String),
    Var(Var),
}

/// Un text cu variabile, de exemplu `D:\Facturi\{created:AAAA}\{created:LL}`.
/// Acoladele literale se scriu `{{` și `}}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Template(pub Vec<Part>);

/// Dicționar: caută un cuvânt în câmpul ales și folosește altul ca rezultat.
/// „Dacă textul conține John, Paul sau George, folosește Beatles.”
#[derive(Debug, Clone, PartialEq)]
pub struct Dictionary {
    pub field: TextField,
    pub entries: Vec<(String, String)>,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    UnclosedBrace,
    UnmatchedBrace,
    UnknownVariable(String),
    UnknownNamed(String),
    /// Dicționarul nu are intrare potrivită și nici valoare implicită.
    MissingValue(String),
}

/// Cum se tratează valorile variabilelor la randare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Valorile devin componente sigure de cale: separatorii și caracterele interzise
    /// pe Windows devin `_`, iar `.` și `..` sunt înlocuite.
    Path,
    /// Ca `Path`, pentru folderul destinație: `{folder}` și `{path}` își păstrează structura.
    /// La începutul șablonului rămân căi absolute (`{folder}\\PDF` = subfolder lângă fișier);
    /// în rest devin o ramură relativă fără literă de unitate
    /// (`D:\\Copie\\{folder}` → `D:\\Copie\\C\\Users\\o\\Downloads`).
    Dir,
    /// Valorile rămân neschimbate (argumentele comenzilor).
    Raw,
    /// Linie pentru `cmd /C`: o valoare în afara ghilimelelor din șablon se pune între
    /// ghilimele; între ghilimele rămâne cum e (fără `"`, care nu apare în nume Windows).
    /// Așa `&`, `|`, `>` dintr-un nume de fișier nu pot începe altă comandă.
    Shell,
}

/// Fusul orar pentru date.
#[derive(Debug, Clone, Copy)]
pub enum Tz {
    /// Decalaj fix față de UTC, în secunde (teste, UTC).
    Fixed(i64),
    /// Decalajul valabil la momentul dat (ora de vară se aplică după data fișierului,
    /// nu după data de azi).
    At(fn(i64) -> i64),
}

impl Tz {
    pub fn offset_at(self, ts: i64) -> i64 {
        match self {
            Tz::Fixed(o) => o,
            Tz::At(f) => f(ts),
        }
    }
}

#[derive(Clone)]
pub struct RenderCtx<'a> {
    pub now: i64,
    pub tz: Tz,
    pub lang: Lang,
    pub dicts: &'a HashMap<String, Dictionary>,
    /// Textul fișierului (în minuscule), pentru dicționarele care caută în conținut;
    /// îl pune motorul doar când o regulă are nevoie de el.
    pub content: Option<Arc<str>>,
}

impl<'a> RenderCtx<'a> {
    pub fn new(now: i64, tz: Tz, lang: Lang, dicts: &'a HashMap<String, Dictionary>) -> Self {
        RenderCtx { now, tz, lang, dicts, content: None }
    }
}

impl Template {
    /// Numele dicționarelor folosite (`{var:Nume}`).
    pub fn named(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|p| match p {
            Part::Var(Var::Named(n)) => Some(n.as_str()),
            _ => None,
        })
    }

    /// Textul sursă, în forma pe care o acceptă `parse` (invers lui `parse`).
    pub fn to_source(&self) -> String {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Part::Lit(l) => out.push_str(&l.replace('{', "{{").replace('}', "}}")),
                Part::Var(v) => {
                    let date = |n: &str, f: &str| format!("{{{n}:{f}}}");
                    out.push_str(&match v {
                        Var::Name => "{name}".into(),
                        Var::Ext => "{ext}".into(),
                        Var::FileName => "{file}".into(),
                        Var::Folder => "{folder}".into(),
                        Var::Path => "{path}".into(),
                        Var::Size => "{size}".into(),
                        Var::Date { src, fmt } => date(
                            match src {
                                DateSrc::Created => "created",
                                DateSrc::Modified => "modified",
                                DateSrc::Now => "now",
                            },
                            fmt,
                        ),
                        Var::Named(n) => format!("{{var:{n}}}"),
                    });
                }
            }
        }
        out
    }

    pub fn literal(s: &str) -> Template {
        Template(vec![Part::Lit(s.to_string())])
    }

    pub fn parse(s: &str) -> Result<Template, TemplateError> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '{' => {
                    if it.peek() == Some(&'{') {
                        it.next();
                        lit.push('{');
                        continue;
                    }
                    let mut tok = String::new();
                    let mut closed = false;
                    for c2 in it.by_ref() {
                        if c2 == '}' {
                            closed = true;
                            break;
                        }
                        tok.push(c2);
                    }
                    if !closed {
                        return Err(TemplateError::UnclosedBrace);
                    }
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Var(parse_var(&tok)?));
                }
                '}' => {
                    if it.peek() == Some(&'}') {
                        it.next();
                        lit.push('}');
                    } else {
                        return Err(TemplateError::UnmatchedBrace);
                    }
                }
                _ => lit.push(c),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Template(parts))
    }

    pub fn render(&self, meta: &FileMeta, ctx: &RenderCtx<'_>, mode: Mode) -> Result<String, TemplateError> {
        let mut out = String::new();
        let mut quoted = false;
        for p in &self.0 {
            match p {
                Part::Lit(s) => {
                    quoted ^= s.matches('"').count() % 2 == 1;
                    out.push_str(s)
                }
                Part::Var(v) => {
                    let val = resolve(v, meta, ctx)?;
                    match mode {
                        Mode::Shell => {
                            let val = val.replace('"', "");
                            if quoted {
                                out.push_str(&val);
                            } else {
                                out.push('"');
                                out.push_str(&val);
                                out.push('"');
                            }
                        }
                        Mode::Dir if matches!(v, Var::Folder | Var::Path) => {
                            if out.is_empty() {
                                out.push_str(&val);
                            } else {
                                out.push_str(&relative_tree(&val));
                            }
                        }
                        Mode::Path | Mode::Dir => out.push_str(&sanitize(&val)),
                        Mode::Raw => out.push_str(&val),
                    }
                }
            }
        }
        Ok(out)
    }
}

fn parse_var(tok: &str) -> Result<Var, TemplateError> {
    let (head, arg) = match tok.split_once(':') {
        Some((h, a)) => (h, Some(a)),
        None => (tok, None),
    };
    let date = |src| {
        arg.map(|f| Var::Date { src, fmt: f.to_string() })
            .ok_or_else(|| TemplateError::UnknownVariable(tok.to_string()))
    };
    match head {
        "name" if arg.is_none() => Ok(Var::Name),
        "ext" if arg.is_none() => Ok(Var::Ext),
        "file" if arg.is_none() => Ok(Var::FileName),
        "folder" if arg.is_none() => Ok(Var::Folder),
        "path" if arg.is_none() => Ok(Var::Path),
        "size" if arg.is_none() => Ok(Var::Size),
        "created" => date(DateSrc::Created),
        "modified" => date(DateSrc::Modified),
        "now" => date(DateSrc::Now),
        "var" => arg
            .filter(|a| !a.is_empty())
            .map(|a| Var::Named(a.to_string()))
            .ok_or_else(|| TemplateError::UnknownVariable(tok.to_string())),
        _ => Err(TemplateError::UnknownVariable(tok.to_string())),
    }
}

fn resolve(v: &Var, meta: &FileMeta, ctx: &RenderCtx<'_>) -> Result<String, TemplateError> {
    Ok(match v {
        Var::Name => meta.stem(),
        Var::Ext => meta.extension(),
        Var::FileName => meta.file_name(),
        Var::Folder => meta.folder(),
        Var::Path => meta.full_path(),
        Var::Size => meta.size.to_string(),
        Var::Date { src, fmt } => {
            let ts = match src {
                DateSrc::Created => meta.created,
                DateSrc::Modified => meta.modified,
                DateSrc::Now => ctx.now,
            };
            timefmt::format(ts, ctx.tz.offset_at(ts), fmt, ctx.lang)
        }
        Var::Named(name) => {
            let dict = ctx.dicts.get(name).ok_or_else(|| TemplateError::UnknownNamed(name.clone()))?;
            // Conținutul vine deja în minuscule (și poate fi mare): nu se copiază.
            let hay: Cow<str> = match dict.field {
                TextField::Content => Cow::Borrowed(ctx.content.as_deref().unwrap_or("")),
                TextField::Name => Cow::Owned(meta.stem().to_lowercase()),
                TextField::Extension => Cow::Owned(meta.extension().to_lowercase()),
                TextField::Path => Cow::Owned(meta.full_path().to_lowercase()),
                TextField::Folder => Cow::Owned(meta.folder().to_lowercase()),
            };
            // O cheie poate avea mai multe variante separate prin virgulă: „pdf, doc, docx”.
            dict.entries
                .iter()
                .find(|(k, _)| k.split(',').map(str::trim).any(|k| !k.is_empty() && hay.contains(&k.to_lowercase())))
                .map(|(_, v)| v.clone())
                .or_else(|| dict.default.clone())
                .ok_or_else(|| TemplateError::MissingValue(name.clone()))?
        }
    })
}

/// O cale absolută ca ramură relativă: `C:\\Users\\o` → `C\\Users\\o`,
/// `\\\\server\\share\\x` → `server\\share\\x`. Fiecare componentă trece prin [`sanitize`].
pub fn relative_tree(val: &str) -> String {
    val.split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .map(|c| sanitize(c.strip_suffix(':').unwrap_or(c)))
        .collect::<Vec<_>>()
        .join(std::path::MAIN_SEPARATOR_STR)
}

/// Numele pe care Windows le refuză sau le modifică în tăcere: puncte și spații la final
/// (se taie), nume de dispozitive (`CON`, `NUL`, `COM1`… și cu extensie: `con.txt`),
/// prea lungi (peste 255 de caractere). Rezultatul nu e niciodată gol.
pub fn safe_name(name: &str) -> String {
    let mut s = name.trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return "_".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(&stem[3..], "¹" | "²" | "³"));
    if reserved {
        s.insert(0, '_');
    }
    if s.chars().count() > 255 {
        // Se scurtează numele, nu extensia.
        let (base, ext) = match s.rfind('.') {
            Some(i) if i > 0 && s.len() - i <= 16 => (s[..i].to_string(), s[i..].to_string()),
            _ => (s.clone(), String::new()),
        };
        let keep = 255 - ext.chars().count();
        s = base.chars().take(keep).collect::<String>().trim_end_matches(['.', ' ']).to_string() + &ext;
    }
    s
}

/// Transformă o valoare într-o componentă sigură de cale.
pub fn sanitize(val: &str) -> String {
    let s: String = val
        .chars()
        .map(|c| if c < ' ' || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    if s == "." || s == ".." {
        "_".to_string()
    } else {
        s
    }
}
