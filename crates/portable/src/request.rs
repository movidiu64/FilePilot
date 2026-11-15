//! Cererile ferestrei către daemon, scrise ca fișiere mici în folderul de date
//! (`request-*.json`). Daemonul le citește când apar (sau la pornire) și le șterge.
//! Așa daemonul, care ține starea fișierelor în memorie, află de „Marchează în așteptare”.

use std::path::PathBuf;

use crate::json::{self, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Regula va procesa din nou aceste fișiere.
    Pending { rule: u64, paths: Vec<PathBuf> },
    /// Regula va procesa din nou toate fișierele.
    PendingRule { rule: u64 },
    /// Confirmările aprobate: pașii de după „Cere confirmare” rulează.
    Approve { ids: Vec<u64> },
    /// Confirmările respinse.
    Reject { ids: Vec<u64> },
}

fn id_str(id: u64) -> Value {
    // Ca text: JavaScript nu poate ține exact numere pe 64 de biți.
    Value::Str(id.to_string())
}

pub fn request_to_json(r: &Request) -> String {
    let v = match r {
        Request::Pending { rule, paths } => Value::Obj(vec![
            ("kind".into(), Value::Str("pending".into())),
            ("rule".into(), id_str(*rule)),
            ("paths".into(), Value::Arr(paths.iter().map(|p| Value::Str(p.to_string_lossy().into_owned())).collect())),
        ]),
        Request::PendingRule { rule } => Value::Obj(vec![
            ("kind".into(), Value::Str("pending_rule".into())),
            ("rule".into(), id_str(*rule)),
        ]),
        Request::Approve { ids } | Request::Reject { ids } => Value::Obj(vec![
            ("kind".into(), Value::Str(if matches!(r, Request::Approve { .. }) { "approve" } else { "reject" }.into())),
            ("ids".into(), Value::Arr(ids.iter().map(|i| id_str(*i)).collect())),
        ]),
    };
    json::to_string_pretty(&v)
}

pub fn parse_request(text: &str) -> Result<Request, String> {
    let v = json::parse(text).map_err(|e| format!("JSON invalid la poziția {}: {}", e.pos, e.msg))?;
    let Value::Obj(o) = v else { return Err("cererea nu e un obiect".into()) };
    let get = |k: &str| o.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    if let Some(Value::Str(k)) = get("kind") {
        if k == "approve" || k == "reject" {
            let Some(Value::Arr(a)) = get("ids") else { return Err("„ids” lipsește".into()) };
            let ids = a
                .iter()
                .map(|x| match x {
                    Value::Str(s) => s.parse::<u64>().map_err(|_| format!("id invalid „{s}”")),
                    Value::Int(n) if *n > 0 => Ok(*n as u64),
                    _ => Err("id invalid".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(if k == "approve" { Request::Approve { ids } } else { Request::Reject { ids } });
        }
    }
    let rule = match get("rule") {
        Some(Value::Str(s)) => s.parse::<u64>().map_err(|_| format!("regulă invalidă „{s}”"))?,
        Some(Value::Int(n)) if *n > 0 => *n as u64,
        _ => return Err("„rule” lipsește".into()),
    };
    match get("kind") {
        Some(Value::Str(k)) if k == "pending" => {
            let Some(Value::Arr(a)) = get("paths") else { return Err("„paths” lipsește".into()) };
            let paths = a
                .iter()
                .map(|x| match x {
                    Value::Str(s) if !s.is_empty() => Ok(PathBuf::from(s)),
                    _ => Err("cale invalidă".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Request::Pending { rule, paths })
        }
        Some(Value::Str(k)) if k == "pending_rule" => Ok(Request::PendingRule { rule }),
        _ => Err("tip de cerere necunoscut".into()),
    }
}
