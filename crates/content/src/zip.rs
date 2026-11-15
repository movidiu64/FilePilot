//! Citirea unui fișier dintr-o arhivă ZIP (directorul central), pentru DOCX, XLSX și ODT.

use crate::inflate::{inflate, MAX_OUT};

fn u16_at(d: &[u8], p: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(d.get(p..p + 2)?.try_into().ok()?)))
}
fn u32_at(d: &[u8], p: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?) as usize)
}

/// O intrare din directorul central.
pub struct Entry {
    pub name: String,
    method: usize,
    csize: usize,
    local: usize,
}

/// Intrările arhivei (fără ZIP64 și fără arhive împărțite).
pub fn entries(d: &[u8]) -> Option<Vec<Entry>> {
    // Sfârșitul directorului central: ultimii 22 de octeți + un comentariu de cel mult 64 KB.
    let min = d.len().saturating_sub(22 + 0xffff);
    let eocd = (min..=d.len().checked_sub(22)?).rev().find(|&p| d[p..p + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let count = u16_at(d, eocd + 10)?;
    let mut p = u32_at(d, eocd + 16)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if d.get(p..p + 4)? != [0x50, 0x4b, 0x01, 0x02] {
            return None;
        }
        let method = u16_at(d, p + 10)?;
        let csize = u32_at(d, p + 20)?;
        let (nlen, xlen, clen) = (u16_at(d, p + 28)?, u16_at(d, p + 30)?, u16_at(d, p + 32)?);
        let local = u32_at(d, p + 42)?;
        let name = String::from_utf8_lossy(d.get(p + 46..p + 46 + nlen)?).into_owned();
        out.push(Entry { name, method, csize, local });
        p += 46 + nlen + xlen + clen;
    }
    Some(out)
}

/// Conținutul unei intrări (stocat sau DEFLATE).
pub fn read(d: &[u8], e: &Entry) -> Option<Vec<u8>> {
    let p = e.local;
    if d.get(p..p + 4)? != [0x50, 0x4b, 0x03, 0x04] {
        return None;
    }
    let start = p + 30 + u16_at(d, p + 26)? + u16_at(d, p + 28)?;
    let data = d.get(start..start.checked_add(e.csize)?)?;
    match e.method {
        0 => Some(data.to_vec()),
        8 => inflate(data, MAX_OUT).ok(),
        _ => None,
    }
}

/// Conținutul intrării cu numele dat.
pub fn read_named(d: &[u8], name: &str) -> Option<Vec<u8>> {
    let es = entries(d)?;
    read(d, es.iter().find(|e| e.name == name)?)
}
