//! Textul din fișiere, pentru condițiile pe conținut: text simplu (UTF-8, UTF-16,
//! Windows-1250), DOCX, XLSX, ODT/ODS/ODP și PDF (fără OCR: un PDF scanat nu are text).
//!
//! Fără dependențe și fără I/O: primește octeții fișierului, întoarce textul.

pub mod inflate;
pub mod pdf;
pub mod xml;
pub mod zip;

/// Cât text se păstrează dintr-un fișier (caractere), ca un document uriaș să nu țină memoria.
pub const MAX_TEXT: usize = 4 << 20;

/// Fișiere care nu au text de căutat (se recunosc după extensie).
const BINARY: &[&str] = &[
    "exe", "dll", "msi", "sys", "bin", "iso", "img", "zip", "rar", "7z", "gz", "tgz", "bz2", "xz", "cab", "jpg", "jpeg",
    "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "ico", "psd", "mp3", "wav", "flac", "ogg", "m4a", "aac", "wma",
    "mp4", "mkv", "avi", "mov", "wmv", "webm", "flv", "doc", "xls", "ppt", "pptx", "dwg", "dxf", "stp", "step", "igs",
    "sldprt", "sldasm", "ttf", "otf", "woff", "woff2", "class", "jar", "apk", "db", "sqlite", "mdb", "accdb", "pst",
];

/// Textul fișierului, după extensie și după conținut; `None` dacă nu are text de citit.
pub fn extract(ext: &str, data: &[u8]) -> Option<String> {
    let ext = ext.to_ascii_lowercase();
    let mut text = match ext.as_str() {
        "docx" | "docm" | "dotx" | "dotm" => xml::docx(data)?,
        "xlsx" | "xlsm" | "xltx" => xml::xlsx(data)?,
        "odt" | "ods" | "odp" | "ott" => xml::odf(data)?,
        "pdf" => pdf::text(data, MAX_TEXT)?,
        e if BINARY.contains(&e) => return None,
        _ if data.starts_with(b"%PDF") => pdf::text(data, MAX_TEXT)?,
        _ if data.starts_with(b"PK\x03\x04") => xml::docx(data).or_else(|| xml::odf(data))?,
        _ => plain(data)?,
    };
    if text.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Some(text)
}

/// Text simplu: BOM UTF-8/UTF-16, UTF-8 valid sau Windows-1250 (diacriticele românești
/// din fișierele vechi). Un fișier cu octeți nuli fără BOM e binar.
pub fn plain(d: &[u8]) -> Option<String> {
    if let Some(r) = d.strip_prefix(b"\xEF\xBB\xBF") {
        return Some(String::from_utf8_lossy(r).into_owned());
    }
    if d.starts_with(b"\xFF\xFE") || d.starts_with(b"\xFE\xFF") {
        let le = d[0] == 0xFF;
        let u: Vec<u16> =
            d[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return Some(String::from_utf16_lossy(&u));
    }
    if d[..d.len().min(8192)].contains(&0) {
        return None;
    }
    match std::str::from_utf8(d) {
        Ok(s) => Some(s.to_string()),
        // Un fișier tăiat la limita de citire poate avea ultimul caracter incomplet.
        Err(e) if e.error_len().is_none() => Some(String::from_utf8_lossy(&d[..e.valid_up_to()]).into_owned()),
        Err(_) => Some(d.iter().map(|&b| if b < 0x80 { b as char } else { char::from_u32(u32::from(CP1250[usize::from(b - 0x80)])).unwrap_or('\u{fffd}') }).collect()),
    }
}

/// Cât să se citească dintr-un fișier: documentele întregi (au nevoie de sfârșit), textul
/// simplu doar începutul. `None` = prea mare sau fără text.
pub fn read_limit(ext: &str, size: u64) -> Option<u64> {
    let ext = ext.to_ascii_lowercase();
    if BINARY.contains(&ext.as_str()) {
        return None;
    }
    match ext.as_str() {
        "docx" | "docm" | "dotx" | "dotm" | "xlsx" | "xlsm" | "xltx" | "odt" | "ods" | "odp" | "ott" | "pdf" => {
            (size <= MAX_DOCUMENT).then_some(size)
        }
        _ => Some(size.min(MAX_PLAIN)),
    }
}

/// Documentele mai mari nu se citesc (condițiile pe conținut le văd ca fără text).
pub const MAX_DOCUMENT: u64 = 64 << 20;
/// Din fișierele text se citește doar începutul.
pub const MAX_PLAIN: u64 = 8 << 20;

const CP1250: [u16; 128] = [
    0x20ac, 0xfffd, 0x201a, 0xfffd, 0x201e, 0x2026, 0x2020, 0x2021,
    0xfffd, 0x2030, 0x0160, 0x2039, 0x015a, 0x0164, 0x017d, 0x0179,
    0xfffd, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0xfffd, 0x2122, 0x0161, 0x203a, 0x015b, 0x0165, 0x017e, 0x017a,
    0x00a0, 0x02c7, 0x02d8, 0x0141, 0x00a4, 0x0104, 0x00a6, 0x00a7,
    0x00a8, 0x00a9, 0x015e, 0x00ab, 0x00ac, 0x00ad, 0x00ae, 0x017b,
    0x00b0, 0x00b1, 0x02db, 0x0142, 0x00b4, 0x00b5, 0x00b6, 0x00b7,
    0x00b8, 0x0105, 0x015f, 0x00bb, 0x013d, 0x02dd, 0x013e, 0x017c,
    0x0154, 0x00c1, 0x00c2, 0x0102, 0x00c4, 0x0139, 0x0106, 0x00c7,
    0x010c, 0x00c9, 0x0118, 0x00cb, 0x011a, 0x00cd, 0x00ce, 0x010e,
    0x0110, 0x0143, 0x0147, 0x00d3, 0x00d4, 0x0150, 0x00d6, 0x00d7,
    0x0158, 0x016e, 0x00da, 0x0170, 0x00dc, 0x00dd, 0x0162, 0x00df,
    0x0155, 0x00e1, 0x00e2, 0x0103, 0x00e4, 0x013a, 0x0107, 0x00e7,
    0x010d, 0x00e9, 0x0119, 0x00eb, 0x011b, 0x00ed, 0x00ee, 0x010f,
    0x0111, 0x0144, 0x0148, 0x00f3, 0x00f4, 0x0151, 0x00f6, 0x00f7,
    0x0159, 0x016f, 0x00fa, 0x0171, 0x00fc, 0x00fd, 0x0163, 0x02d9,
];
pub(crate) const CP1252: [u16; 128] = [
    0x20ac, 0xfffd, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021,
    0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0xfffd, 0x017d, 0xfffd,
    0xfffd, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0xfffd, 0x017e, 0x0178,
    0x00a0, 0x00a1, 0x00a2, 0x00a3, 0x00a4, 0x00a5, 0x00a6, 0x00a7,
    0x00a8, 0x00a9, 0x00aa, 0x00ab, 0x00ac, 0x00ad, 0x00ae, 0x00af,
    0x00b0, 0x00b1, 0x00b2, 0x00b3, 0x00b4, 0x00b5, 0x00b6, 0x00b7,
    0x00b8, 0x00b9, 0x00ba, 0x00bb, 0x00bc, 0x00bd, 0x00be, 0x00bf,
    0x00c0, 0x00c1, 0x00c2, 0x00c3, 0x00c4, 0x00c5, 0x00c6, 0x00c7,
    0x00c8, 0x00c9, 0x00ca, 0x00cb, 0x00cc, 0x00cd, 0x00ce, 0x00cf,
    0x00d0, 0x00d1, 0x00d2, 0x00d3, 0x00d4, 0x00d5, 0x00d6, 0x00d7,
    0x00d8, 0x00d9, 0x00da, 0x00db, 0x00dc, 0x00dd, 0x00de, 0x00df,
    0x00e0, 0x00e1, 0x00e2, 0x00e3, 0x00e4, 0x00e5, 0x00e6, 0x00e7,
    0x00e8, 0x00e9, 0x00ea, 0x00eb, 0x00ec, 0x00ed, 0x00ee, 0x00ef,
    0x00f0, 0x00f1, 0x00f2, 0x00f3, 0x00f4, 0x00f5, 0x00f6, 0x00f7,
    0x00f8, 0x00f9, 0x00fa, 0x00fb, 0x00fc, 0x00fd, 0x00fe, 0x00ff,
];
