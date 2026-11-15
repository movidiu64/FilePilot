//! Textul din documentele Office (DOCX, XLSX) și OpenDocument (ODT, ODS): XML din arhive ZIP.

use crate::zip;

/// Decodează entitățile XML (`&amp;`, `&#259;`, `&#x103;`).
pub fn unescape(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.as_bytes()[..rest.len().min(12)].iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

/// Cum se transformă etichetele în text.
pub struct Style {
    /// Elementele al căror conținut e text (`w:t`, `t`, `text:span`...). Gol = tot textul dintre etichete.
    pub text_tags: &'static [&'static str],
    /// Etichete (de deschidere sau goale) care înseamnă tab.
    pub tab: &'static [&'static str],
    /// Etichete care înseamnă rând nou (deschidere, închidere sau goale).
    pub newline: &'static [&'static str],
}

pub const WORD: Style = Style { text_tags: &["w:t"], tab: &["w:tab"], newline: &["w:br", "w:cr", "/w:p"] };
pub const XLSX_STRINGS: Style = Style { text_tags: &["t"], tab: &[], newline: &["/si"] };
pub const ODF: Style = Style { text_tags: &[], tab: &["text:tab"], newline: &["/text:p", "/text:h", "text:line-break"] };

/// Textul unui document XML, după stil.
pub fn text(xml: &str, st: &Style, out: &mut String) {
    let is = |list: &[&str], name: &str, closing: bool| {
        list.iter().any(|n| match n.strip_prefix('/') {
            Some(c) => closing && c == name,
            None => !closing && *n == name,
        })
    };
    let mut rest = xml;
    let mut inside = st.text_tags.is_empty();
    while let Some(lt) = rest.find('<') {
        if inside {
            unescape(&rest[..lt], out);
        }
        rest = &rest[lt + 1..];
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[..gt];
        rest = &rest[gt + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let t = tag.trim_start_matches('/');
        let name = &t[..t.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(t.len())];
        if is(st.newline, name, closing) {
            out.push('\n');
        }
        if is(st.tab, name, closing) {
            out.push('\t');
        }
        if !st.text_tags.is_empty() && st.text_tags.contains(&name) {
            inside = !closing && !tag.ends_with('/');
        }
    }
}

/// DOCX: corpul, antetele, subsolurile și notele de subsol.
pub fn docx(d: &[u8]) -> Option<String> {
    let es = zip::entries(d)?;
    let mut parts: Vec<&zip::Entry> = es
        .iter()
        .filter(|e| {
            e.name == "word/document.xml"
                || e.name.starts_with("word/header")
                || e.name.starts_with("word/footer")
                || e.name == "word/footnotes.xml"
                || e.name == "word/endnotes.xml"
        })
        .collect();
    if !parts.iter().any(|e| e.name == "word/document.xml") {
        return None;
    }
    // Corpul întâi, ca „prima apariție” din text să fie din document.
    parts.sort_by_key(|e| e.name != "word/document.xml");
    let mut out = String::new();
    for e in parts {
        if let Some(x) = zip::read(d, e) {
            text(&String::from_utf8_lossy(&x), &WORD, &mut out);
            out.push('\n');
        }
    }
    Some(out)
}

/// XLSX: textele comune (o dată) și valorile din foi care nu sunt trimiteri la ele
/// (numere, rezultatele formulelor, texte scrise direct în celulă).
pub fn xlsx(d: &[u8]) -> Option<String> {
    let es = zip::entries(d)?;
    let mut out = String::new();
    if let Some(e) = es.iter().find(|e| e.name == "xl/sharedStrings.xml") {
        text(&String::from_utf8_lossy(&zip::read(d, e)?), &XLSX_STRINGS, &mut out);
    }
    let mut sheets: Vec<&zip::Entry> = es.iter().filter(|e| e.name.starts_with("xl/worksheets/sheet")).collect();
    sheets.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    for e in sheets {
        let Some(x) = zip::read(d, e) else { continue };
        let x = String::from_utf8_lossy(&x);
        out.push('\n');
        let mut rest = &x[..];
        while let Some(i) = rest.find("<c ") {
            // Rând nou între celule.
            if rest[..i].contains("</row>") && !out.ends_with('\n') {
                out.push('\n');
            }
            rest = &rest[i..];
            let Some(gt) = rest.find('>') else { break };
            let open = &rest[..gt];
            if open.ends_with('/') {
                rest = &rest[gt..];
                continue;
            }
            let Some(end) = rest[gt..].find("</c>").map(|e| e + gt) else { break };
            let cell = &rest[gt + 1..end];
            rest = &rest[end + 4..];
            if open.contains(" t=\"s\"") {
                continue;
            }
            for tag in ["<v>", "<t>", "<t "] {
                if let Some(a) = cell.find(tag) {
                    let a = a + cell[a..].find('>').unwrap_or(0) + 1;
                    if let Some(b) = cell[a..].find("</") {
                        unescape(&cell[a..a + b], &mut out);
                        out.push('\t');
                    }
                    break;
                }
            }
        }
    }
    Some(out)
}

/// ODT, ODS, ODP: `content.xml`.
pub fn odf(d: &[u8]) -> Option<String> {
    let x = zip::read_named(d, "content.xml")?;
    let s = String::from_utf8_lossy(&x);
    // Doar corpul documentului (fără stilurile automate).
    let body = s.find("<office:body").map_or(&s[..], |i| &s[i..]);
    let mut out = String::new();
    text(body, &ODF, &mut out);
    Some(out)
}
