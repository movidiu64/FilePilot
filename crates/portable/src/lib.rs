//! Import/export de reguli (`.orgrules`, JSON). Fișierul nu conține id-uri: la import,
//! apelantul le atribuie. Regulile ascunse se exportă doar la cerere explicită.

pub mod json;
pub mod request;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use json::Value;
use org_core::*;

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    Json(json::ParseError),
    /// Fișier făcut de o versiune mai nouă a aplicației.
    NewerVersion(i64),
    /// (context, mesaj)
    Invalid(String, String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Json(e) => write!(f, "JSON invalid la poziția {}: {}", e.pos, e.msg),
            ImportError::NewerVersion(v) => write!(f, "fișier de versiune mai nouă ({v})"),
            ImportError::Invalid(c, m) => write!(f, "{c}: {m}"),
        }
    }
}

impl std::error::Error for ImportError {}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bundle {
    /// Cu `id` = 0; apelantul le atribuie.
    pub rules: Vec<Rule>,
    pub dictionaries: BTreeMap<String, Dictionary>,
}

// ---------- export ----------

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}
fn obj(v: Vec<(&str, Value)>) -> Value {
    Value::Obj(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn field_name(f: TextField) -> &'static str {
    match f {
        TextField::Name => "name",
        TextField::Extension => "extension",
        TextField::Path => "path",
        TextField::Folder => "folder",
        TextField::Content => "content",
    }
}

fn text_op_name(o: TextOp) -> &'static str {
    match o {
        TextOp::Is => "is",
        TextOp::IsNot => "is_not",
        TextOp::Contains => "contains",
        TextOp::DoesNotContain => "does_not_contain",
        TextOp::IsNothing => "is_nothing",
        TextOp::StartsWith => "starts_with",
        TextOp::EndsWith => "ends_with",
    }
}

fn conflict_name(c: Conflict) -> &'static str {
    match c {
        Conflict::Rename => "rename",
        Conflict::Skip => "skip",
        Conflict::Overwrite => "overwrite",
        Conflict::Dedupe => "dedupe",
    }
}

fn folder_value(f: &WatchedFolder) -> Value {
    obj(vec![("path", s(&f.path.to_string_lossy())), ("recursive", Value::Bool(f.recursive))])
}

fn folder_from(f: &Value, ctx: &str) -> R<WatchedFolder> {
    let fo = as_obj(f, ctx)?;
    Ok(WatchedFolder { path: PathBuf::from(req_str(fo, "path", ctx)?), recursive: opt_bool(fo, "recursive", true, ctx)? })
}

fn cond_to_value(c: &Condition) -> Value {
    match c {
        Condition::AllFiles => obj(vec![("type", s("all_files"))]),
        Condition::All(v) => obj(vec![("type", s("all")), ("items", Value::Arr(v.iter().map(cond_to_value).collect()))]),
        Condition::Any(v) => obj(vec![("type", s("any")), ("items", Value::Arr(v.iter().map(cond_to_value).collect()))]),
        Condition::Text { field, op, value } => obj(vec![
            ("type", s("text")),
            ("field", s(field_name(*field))),
            ("op", s(text_op_name(*op))),
            ("value", s(value)),
        ]),
        Condition::Duplicate { is, within } => {
            let mut v = vec![("type", s("duplicate")), ("is", Value::Bool(*is))];
            // Doar dacă există: fișierele vechi rămân la fel.
            if !within.is_empty() {
                v.push(("within", Value::Arr(within.iter().map(folder_value).collect())));
            }
            obj(v)
        }
        Condition::Size { op, bytes } => obj(vec![
            ("type", s("size")),
            ("op", s(if *op == SizeOp::Larger { "larger" } else { "smaller" })),
            ("bytes", Value::Int(*bytes as i64)),
        ]),
        Condition::Date { field, op } => {
            let (name, v) = match op {
                DateOp::Before(t) => ("before", *t),
                DateOp::After(t) => ("after", *t),
                DateOp::OlderThanDays(d) => ("older_than_days", *d as i64),
                DateOp::WithinDays(d) => ("within_days", *d as i64),
            };
            obj(vec![
                ("type", s("date")),
                ("field", s(if *field == DateField::Created { "created" } else { "modified" })),
                ("op", s(name)),
                ("value", Value::Int(v)),
            ])
        }
    }
}

fn action_to_value(a: &Action) -> Value {
    match a {
        Action::Move { dest, conflict, keep_structure, remove_empty } => obj(vec![
            ("type", s("move")),
            ("dest", s(&dest.to_source())),
            ("conflict", s(conflict_name(*conflict))),
            ("keep_structure", Value::Bool(*keep_structure)),
            ("remove_empty", Value::Bool(*remove_empty)),
        ]),
        Action::Copy { dest, conflict, keep_structure, then_target } => obj(vec![
            ("type", s("copy")),
            ("dest", s(&dest.to_source())),
            ("conflict", s(conflict_name(*conflict))),
            ("keep_structure", Value::Bool(*keep_structure)),
            ("then_target", Value::Bool(*then_target)),
        ]),
        Action::Rename { name, conflict } => obj(vec![
            ("type", s("rename")),
            ("name", s(&name.to_source())),
            ("conflict", s(conflict_name(*conflict))),
        ]),
        Action::ChangeExtension { ext } => obj(vec![("type", s("change_extension")), ("ext", s(&ext.to_source()))]),
        Action::Trash { remove_empty } => obj(vec![("type", s("trash")), ("remove_empty", Value::Bool(*remove_empty))]),
        Action::Confirm => obj(vec![("type", s("confirm"))]),
        Action::Extract { dest, conflict } => obj(vec![
            ("type", s("extract")),
            ("dest", s(&dest.to_source())),
            ("conflict", s(conflict_name(*conflict))),
        ]),
        Action::Delete { remove_empty } => obj(vec![("type", s("delete")), ("remove_empty", Value::Bool(*remove_empty))]),
        Action::Command { line } => obj(vec![("type", s("command")), ("line", s(&line.to_source()))]),
        Action::Run { program, args } => obj(vec![
            ("type", s("run")),
            ("program", s(&program.to_source())),
            ("args", Value::Arr(args.iter().map(|t| s(&t.to_source())).collect())),
        ]),
    }
}

/// Exportă regulile și dicționarele. Regulile ascunse se includ doar cu `include_hidden`
/// (adică aplicația e deblocată și utilizatorul a cerut asta).
pub fn export(rules: &[Rule], dicts: &BTreeMap<String, Dictionary>, include_hidden: bool) -> String {
    let rules_v: Vec<Value> = rules
        .iter()
        .filter(|r| include_hidden || !r.hidden)
        .map(|r| {
            let mut fields = Vec::new();
            if r.id != 0 {
                fields.push(("id", s(&r.id.to_string())));
            }
            fields.extend([
                ("name", s(&r.name)),
                ("enabled", Value::Bool(r.enabled)),
                ("hidden", Value::Bool(r.hidden)),
                ("continue_after_move", Value::Bool(r.continue_after_move)),
                ("simulate", Value::Bool(r.simulate)),
                (
                    "folders",
                    Value::Arr(
                        r.folders.iter().map(folder_value).collect(),
                    ),
                ),
                ("condition", cond_to_value(&r.condition)),
                ("actions", Value::Arr(r.actions.iter().map(action_to_value).collect())),
            ]);
            obj(fields)
        })
        .collect();
    let dicts_v = Value::Obj(
        dicts
            .iter()
            .map(|(name, d)| {
                (
                    name.clone(),
                    obj(vec![
                        ("field", s(field_name(d.field))),
                        (
                            "entries",
                            Value::Arr(d.entries.iter().map(|(k, v)| Value::Arr(vec![s(k), s(v)])).collect()),
                        ),
                        ("default", d.default.as_deref().map_or(Value::Null, s)),
                    ]),
                )
            })
            .collect(),
    );
    json::to_string_pretty(&obj(vec![
        ("app", s("filepilot")),
        ("schema_version", Value::Int(SCHEMA_VERSION)),
        ("rules", Value::Arr(rules_v)),
        ("dictionaries", dicts_v),
    ]))
}

// ---------- import ----------

type R<T> = Result<T, ImportError>;

fn bad<T>(ctx: &str, msg: impl Into<String>) -> R<T> {
    Err(ImportError::Invalid(ctx.to_string(), msg.into()))
}

fn get<'a>(o: &'a [(String, Value)], k: &str) -> Option<&'a Value> {
    o.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn as_obj<'a>(v: &'a Value, ctx: &str) -> R<&'a [(String, Value)]> {
    match v {
        Value::Obj(o) => Ok(o),
        _ => bad(ctx, "se aștepta un obiect"),
    }
}

fn req_str<'a>(o: &'a [(String, Value)], k: &str, ctx: &str) -> R<&'a str> {
    match get(o, k) {
        Some(Value::Str(x)) => Ok(x),
        _ => bad(ctx, format!("câmpul „{k}” lipsește sau nu e text")),
    }
}

fn opt_bool(o: &[(String, Value)], k: &str, default: bool, ctx: &str) -> R<bool> {
    match get(o, k) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        _ => bad(ctx, format!("câmpul „{k}” trebuie să fie true/false")),
    }
}

fn req_int(o: &[(String, Value)], k: &str, ctx: &str) -> R<i64> {
    match get(o, k) {
        Some(Value::Int(i)) => Ok(*i),
        _ => bad(ctx, format!("câmpul „{k}” lipsește sau nu e număr")),
    }
}

fn parse_field(x: &str, ctx: &str) -> R<TextField> {
    Ok(match x {
        "name" => TextField::Name,
        "extension" => TextField::Extension,
        "path" => TextField::Path,
        "folder" => TextField::Folder,
        "content" => TextField::Content,
        _ => return bad(ctx, format!("câmp necunoscut „{x}”")),
    })
}

fn parse_conflict(o: &[(String, Value)], ctx: &str) -> R<Conflict> {
    match get(o, "conflict") {
        None => Ok(Conflict::Rename),
        Some(Value::Str(x)) => Ok(match x.as_str() {
            "rename" => Conflict::Rename,
            "skip" => Conflict::Skip,
            "overwrite" => Conflict::Overwrite,
            "dedupe" => Conflict::Dedupe,
            _ => return bad(ctx, format!("conflict necunoscut „{x}”")),
        }),
        _ => bad(ctx, "conflict invalid"),
    }
}

fn tpl(o: &[(String, Value)], k: &str, ctx: &str) -> R<Template> {
    let src = req_str(o, k, ctx)?;
    Template::parse(src).or_else(|e| bad(ctx, format!("șablon invalid în „{k}”: {e:?}")))
}

fn cond_from(v: &Value, depth: usize) -> R<Condition> {
    let ctx = "condiție";
    if depth > MAX_DEPTH + 1 {
        return bad(ctx, "imbricare prea adâncă");
    }
    let o = as_obj(v, ctx)?;
    Ok(match req_str(o, "type", ctx)? {
        "all_files" => Condition::AllFiles,
        t @ ("all" | "any") => {
            let Some(Value::Arr(items)) = get(o, "items") else { return bad(ctx, "„items” lipsește") };
            let items = items.iter().map(|i| cond_from(i, depth + 1)).collect::<R<Vec<_>>>()?;
            if t == "all" { Condition::All(items) } else { Condition::Any(items) }
        }
        "text" => {
            let op = match req_str(o, "op", ctx)? {
                "is" => TextOp::Is,
                "is_not" => TextOp::IsNot,
                "contains" => TextOp::Contains,
                "does_not_contain" => TextOp::DoesNotContain,
                "is_nothing" => TextOp::IsNothing,
                "starts_with" => TextOp::StartsWith,
                "ends_with" => TextOp::EndsWith,
                x => return bad(ctx, format!("operator necunoscut „{x}”")),
            };
            Condition::Text {
                field: parse_field(req_str(o, "field", ctx)?, ctx)?,
                op,
                value: get(o, "value").and_then(|x| if let Value::Str(s) = x { Some(s.clone()) } else { None }).unwrap_or_default(),
            }
        }
        "duplicate" => Condition::Duplicate {
            is: opt_bool(o, "is", true, ctx)?,
            within: match get(o, "within") {
                Some(Value::Arr(fs)) => fs
                    .iter()
                    .map(|f| folder_from(f, ctx))
                    .filter(|f| !matches!(f, Ok(w) if w.path.as_os_str().is_empty()))
                    .collect::<R<Vec<_>>>()?,
                _ => Vec::new(),
            },
        },
        "size" => {
            let op = match req_str(o, "op", ctx)? {
                "larger" => SizeOp::Larger,
                "smaller" => SizeOp::Smaller,
                x => return bad(ctx, format!("operator necunoscut „{x}”")),
            };
            let b = req_int(o, "bytes", ctx)?;
            if b < 0 {
                return bad(ctx, "dimensiune negativă");
            }
            Condition::Size { op, bytes: b as u64 }
        }
        "date" => {
            let field = match req_str(o, "field", ctx)? {
                "created" => DateField::Created,
                "modified" => DateField::Modified,
                x => return bad(ctx, format!("câmp de dată necunoscut „{x}”")),
            };
            let val = req_int(o, "value", ctx)?;
            let days = || u32::try_from(val).or_else(|_| bad(ctx, "număr de zile invalid"));
            let op = match req_str(o, "op", ctx)? {
                "before" => DateOp::Before(val),
                "after" => DateOp::After(val),
                "older_than_days" => DateOp::OlderThanDays(days()?),
                "within_days" => DateOp::WithinDays(days()?),
                x => return bad(ctx, format!("operator necunoscut „{x}”")),
            };
            Condition::Date { field, op }
        }
        x => return bad(ctx, format!("tip de condiție necunoscut „{x}”")),
    })
}

fn action_from(v: &Value) -> R<Action> {
    let ctx = "acțiune";
    let o = as_obj(v, ctx)?;
    Ok(match req_str(o, "type", ctx)? {
        "move" => Action::Move {
            dest: tpl(o, "dest", ctx)?,
            conflict: parse_conflict(o, ctx)?,
            keep_structure: opt_bool(o, "keep_structure", false, ctx)?,
            remove_empty: opt_bool(o, "remove_empty", false, ctx)?,
        },
        "copy" => Action::Copy {
            dest: tpl(o, "dest", ctx)?,
            conflict: parse_conflict(o, ctx)?,
            keep_structure: opt_bool(o, "keep_structure", false, ctx)?,
            then_target: opt_bool(o, "then_target", false, ctx)?,
        },
        "rename" => Action::Rename { name: tpl(o, "name", ctx)?, conflict: parse_conflict(o, ctx)? },
        "change_extension" => Action::ChangeExtension { ext: tpl(o, "ext", ctx)? },
        "trash" => Action::Trash { remove_empty: opt_bool(o, "remove_empty", false, ctx)? },
        "confirm" => Action::Confirm,
        "extract" => Action::Extract { dest: tpl(o, "dest", ctx)?, conflict: parse_conflict(o, ctx)? },
        "delete" => Action::Delete { remove_empty: opt_bool(o, "remove_empty", false, ctx)? },
        "run" => {
            let Some(Value::Arr(args)) = get(o, "args") else { return bad(ctx, "„args” lipsește") };
            let args = args
                .iter()
                .map(|a| match a {
                    Value::Str(x) => Template::parse(x).or_else(|e| bad(ctx, format!("șablon invalid: {e:?}"))),
                    _ => bad(ctx, "argumentele trebuie să fie text"),
                })
                .collect::<R<Vec<_>>>()?;
            Action::Run { program: tpl(o, "program", ctx)?, args }
        }
        "command" => Action::Command { line: tpl(o, "line", ctx)? },
        x => return bad(ctx, format!("tip de acțiune necunoscut „{x}”")),
    })
}

fn rule_from(v: &Value, index: usize) -> R<Rule> {
    let o = as_obj(v, "regulă")?;
    let name = req_str(o, "name", &format!("regula #{}", index + 1))?.to_string();
    let ctx = format!("regula „{name}”");
    let with_ctx = |e: ImportError| match e {
        ImportError::Invalid(c, m) => ImportError::Invalid(format!("{ctx} / {c}"), m),
        other => other,
    };
    let Some(Value::Arr(fs)) = get(o, "folders") else { return bad(&ctx, "„folders” lipsește") };
    let folders = fs.iter().map(|f| folder_from(f, &ctx)).collect::<R<Vec<_>>>()?;
    let Some(Value::Arr(acts)) = get(o, "actions") else { return bad(&ctx, "„actions” lipsește") };
    let actions = acts.iter().map(action_from).collect::<R<Vec<_>>>().map_err(with_ctx)?;
    let condition = cond_from(get(o, "condition").unwrap_or(&Value::Null), 0).map_err(with_ctx)?;
    // Id permanent (text, ca JavaScript să nu-l rotunjească). Lipsă = se atribuie la încărcare.
    let id = match get(o, "id") {
        None | Some(Value::Null) => 0,
        Some(Value::Str(x)) => match x.parse::<u64>() {
            Ok(n) if n > 0 && n <= i64::MAX as u64 => n,
            _ => return bad(&ctx, format!("id invalid „{x}”")),
        },
        Some(Value::Int(n)) if *n > 0 => *n as u64,
        Some(_) => return bad(&ctx, "„id” trebuie să fie text cu cifre"),
    };
    let rule = Rule {
        id,
        name,
        enabled: opt_bool(o, "enabled", true, &ctx)?,
        hidden: opt_bool(o, "hidden", false, &ctx)?,
        folders,
        condition,
        actions,
        continue_after_move: opt_bool(o, "continue_after_move", true, &ctx)?,
        simulate: opt_bool(o, "simulate", false, &ctx)?,
    };
    // O regulă oprită poate fi încă nefinalizată (în lucru în interfață); una activă trebuie să fie validă.
    if rule.enabled {
        rule.validate().or_else(|e| bad(&ctx, format!("regulă invalidă: {e:?}")))?;
    }
    Ok(rule)
}

pub fn import(text: &str) -> R<Bundle> {
    let root = json::parse(text).map_err(ImportError::Json)?;
    let o = as_obj(&root, "fișier")?;
    match get(o, "app") {
        // "organizator": files exported before the rename.
        Some(Value::Str(a)) if a == "filepilot" || a == "organizator" => {}
        _ => return bad("fișier", "nu e un fișier .orgrules"),
    }
    let ver = req_int(o, "schema_version", "fișier")?;
    if ver > SCHEMA_VERSION {
        return Err(ImportError::NewerVersion(ver));
    }
    let rules = match get(o, "rules") {
        Some(Value::Arr(a)) => a.iter().enumerate().map(|(i, r)| rule_from(r, i)).collect::<R<Vec<_>>>()?,
        _ => return bad("fișier", "„rules” lipsește"),
    };
    let mut dictionaries = BTreeMap::new();
    if let Some(Value::Obj(ds)) = get(o, "dictionaries") {
        for (name, dv) in ds {
            let ctx = format!("dicționar „{name}”");
            let d = as_obj(dv, &ctx)?;
            let Some(Value::Arr(es)) = get(d, "entries") else { return bad(&ctx, "„entries” lipsește") };
            let entries = es
                .iter()
                .map(|e| match e {
                    Value::Arr(p) if p.len() == 2 => match (&p[0], &p[1]) {
                        (Value::Str(k), Value::Str(v)) => Ok((k.clone(), v.clone())),
                        _ => bad(&ctx, "intrare invalidă"),
                    },
                    _ => bad(&ctx, "intrare invalidă"),
                })
                .collect::<R<Vec<_>>>()?;
            let default = match get(d, "default") {
                Some(Value::Str(x)) => Some(x.clone()),
                _ => None,
            };
            dictionaries.insert(
                name.clone(),
                Dictionary { field: parse_field(req_str(d, "field", &ctx)?, &ctx)?, entries, default },
            );
        }
    }
    Ok(Bundle { rules, dictionaries })
}

// ---------- id-uri stabile ----------

/// Id stabil din numele regulii (FNV-1a redus la 63 de biți, ca să încapă în INTEGER-ul
/// SQLite). Starea salvată rămâne valabilă între porniri cât timp numele nu se schimbă.
/// Numele identice primesc id-uri consecutive, în ordinea din fișier.
pub fn stable_id(name: &str, taken: &HashSet<u64>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h &= i64::MAX as u64;
    while taken.contains(&h) {
        h = (h + 1) & i64::MAX as u64;
    }
    h
}

/// Atribuie id-uri. Un id deja salvat în fișier rămâne (așa redenumirea unei reguli nu o
/// face să proceseze din nou fișierele). Regulile fără id, sau cu un id deja folosit de o
/// regulă de mai sus (duplicat, import), primesc unul derivat din nume — exact cel pe care
/// îl aveau înainte de id-urile permanente, deci starea salvată rămâne valabilă.
pub fn assign_ids(rules: &mut [Rule]) {
    let mut taken = HashSet::new();
    let mut keep = vec![false; rules.len()];
    for (i, r) in rules.iter().enumerate() {
        if r.id != 0 && taken.insert(r.id) {
            keep[i] = true;
        }
    }
    for (i, r) in rules.iter_mut().enumerate() {
        if !keep[i] {
            r.id = stable_id(&r.name, &taken);
            taken.insert(r.id);
        }
    }
}

/// La salvarea din fereastra blocată (fără regulile ascunse): pune regulile ascunse de pe
/// disc înapoi, pe pozițiile lor față de celelalte. Un id luat de la o regulă ascunsă
/// (de exemplu dintr-un JSON lipit) se eliberează, ca regula ascunsă să-l păstreze.
pub fn merge_hidden(visible: Vec<Rule>, disk: &[Rule]) -> Vec<Rule> {
    let hidden_ids: HashSet<u64> = disk.iter().filter(|r| r.hidden).map(|r| r.id).collect();
    let mut incoming = visible.into_iter().map(|mut r| {
        if hidden_ids.contains(&r.id) {
            r.id = 0;
        }
        r
    });
    let mut out = Vec::new();
    for d in disk {
        if d.hidden {
            out.push(d.clone());
        } else if let Some(v) = incoming.next() {
            out.push(v);
        }
    }
    out.extend(incoming);
    out
}
