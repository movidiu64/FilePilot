//! Textul dintr-un PDF: obiectele (și fluxurile de obiecte), paginile în ordine, fonturile
//! cu `ToUnicode` sau codări simple, operatorii de text. Fără randare: ordinea e cea din
//! fluxul paginii, ceea ce ajunge pentru căutare. PDF-urile criptate nu se citesc.

use std::collections::{HashMap, HashSet};

use crate::inflate::{zlib_partial, MAX_OUT};
use crate::CP1252;

#[derive(Debug, Clone, PartialEq)]
enum Obj {
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Arr(Vec<Obj>),
    Dict(Vec<(Vec<u8>, Obj)>),
    Ref(u32),
    /// Dicționarul și datele brute (încă codate).
    Stream(Vec<(Vec<u8>, Obj)>, Vec<u8>),
}

impl Obj {
    fn dict(&self) -> Option<&[(Vec<u8>, Obj)]> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _) => Some(d),
            _ => None,
        }
    }
    fn get(&self, k: &[u8]) -> Option<&Obj> {
        self.dict()?.iter().find(|(x, _)| x == k).map(|(_, v)| v)
    }
    fn name(&self) -> Option<&[u8]> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            Obj::Num(n) => Some(*n),
            _ => None,
        }
    }
}

// ---------- lexer ----------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64, bool),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Kw(Vec<u8>),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
}

fn is_ws(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}
fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Lexer<'a> {
    fn new(d: &'a [u8], p: usize) -> Self {
        Lexer { d, p }
    }

    fn skip_ws(&mut self) {
        while self.p < self.d.len() {
            let c = self.d[self.p];
            if is_ws(c) {
                self.p += 1;
            } else if c == b'%' {
                while self.p < self.d.len() && self.d[self.p] != b'\n' && self.d[self.p] != b'\r' {
                    self.p += 1;
                }
            } else {
                break;
            }
        }
    }

    fn next(&mut self) -> Option<Tok> {
        self.skip_ws();
        let d = self.d;
        let c = *d.get(self.p)?;
        self.p += 1;
        match c {
            b'[' => Some(Tok::ArrOpen),
            b']' => Some(Tok::ArrClose),
            b'{' | b'}' => self.next(),
            b'<' if d.get(self.p) == Some(&b'<') => {
                self.p += 1;
                Some(Tok::DictOpen)
            }
            b'>' if d.get(self.p) == Some(&b'>') => {
                self.p += 1;
                Some(Tok::DictClose)
            }
            b'>' => self.next(),
            b'<' => {
                let mut out = Vec::new();
                let mut hi: Option<u8> = None;
                while let Some(&c) = d.get(self.p) {
                    self.p += 1;
                    if c == b'>' {
                        break;
                    }
                    let v = match c {
                        b'0'..=b'9' => c - b'0',
                        b'a'..=b'f' => c - b'a' + 10,
                        b'A'..=b'F' => c - b'A' + 10,
                        _ => continue,
                    };
                    match hi.take() {
                        Some(h) => out.push(h << 4 | v),
                        None => hi = Some(v),
                    }
                }
                if let Some(h) = hi {
                    out.push(h << 4);
                }
                Some(Tok::Str(out))
            }
            b'(' => {
                let mut out = Vec::new();
                let mut depth = 1;
                while let Some(&c) = d.get(self.p) {
                    self.p += 1;
                    match c {
                        b'(' => {
                            depth += 1;
                            out.push(c);
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            out.push(c);
                        }
                        b'\\' => {
                            let Some(&e) = d.get(self.p) else { break };
                            self.p += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b'r' => out.push(b'\r'),
                                b't' => out.push(b'\t'),
                                b'b' => out.push(8),
                                b'f' => out.push(12),
                                b'\r' => {
                                    if d.get(self.p) == Some(&b'\n') {
                                        self.p += 1;
                                    }
                                }
                                b'\n' => {}
                                b'0'..=b'7' => {
                                    let mut v = u32::from(e - b'0');
                                    for _ in 0..2 {
                                        match d.get(self.p) {
                                            Some(&o @ b'0'..=b'7') => {
                                                v = v * 8 + u32::from(o - b'0');
                                                self.p += 1;
                                            }
                                            _ => break,
                                        }
                                    }
                                    out.push(v as u8);
                                }
                                _ => out.push(e),
                            }
                        }
                        _ => out.push(c),
                    }
                }
                Some(Tok::Str(out))
            }
            b'/' => {
                let mut out = Vec::new();
                while let Some(&c) = d.get(self.p) {
                    if is_ws(c) || is_delim(c) {
                        break;
                    }
                    self.p += 1;
                    if c == b'#' {
                        if let Some(v) = d.get(self.p..self.p + 2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                            out.push(v);
                            self.p += 2;
                            continue;
                        }
                    }
                    out.push(c);
                }
                Some(Tok::Name(out))
            }
            _ => {
                let start = self.p - 1;
                while let Some(&c) = d.get(self.p) {
                    if is_ws(c) || is_delim(c) {
                        break;
                    }
                    self.p += 1;
                }
                let w = &d[start..self.p];
                if matches!(w[0], b'0'..=b'9' | b'+' | b'-' | b'.') {
                    if let Some(n) = std::str::from_utf8(w).ok().and_then(|s| s.parse::<f64>().ok()) {
                        return Some(Tok::Num(n, !w.contains(&b'.')));
                    }
                }
                Some(Tok::Kw(w.to_vec()))
            }
        }
    }

    /// O valoare completă (cu `N G R` ca trimitere).
    fn value(&mut self, depth: usize) -> Option<Obj> {
        if depth > 64 {
            return None;
        }
        let t = self.next()?;
        Some(match t {
            Tok::Num(n, int) => {
                if int && n >= 0.0 {
                    let save = self.p;
                    if let Some(Tok::Num(_, true)) = self.next() {
                        if self.next() == Some(Tok::Kw(b"R".to_vec())) {
                            return Some(Obj::Ref(n as u32));
                        }
                    }
                    self.p = save;
                }
                Obj::Num(n)
            }
            Tok::Str(s) => Obj::Str(s),
            Tok::Name(n) => Obj::Name(n),
            Tok::Kw(k) => match &k[..] {
                b"true" => Obj::Bool(true),
                b"false" => Obj::Bool(false),
                _ => Obj::Null,
            },
            Tok::ArrOpen => {
                let mut v = Vec::new();
                loop {
                    self.skip_ws();
                    if self.d.get(self.p) == Some(&b']') {
                        self.p += 1;
                        break;
                    }
                    if self.p >= self.d.len() {
                        break;
                    }
                    v.push(self.value(depth + 1)?);
                }
                Obj::Arr(v)
            }
            Tok::DictOpen => {
                let mut v = Vec::new();
                loop {
                    match self.next()? {
                        Tok::DictClose => break,
                        Tok::Name(k) => {
                            self.skip_ws();
                            if self.d.get(self.p..self.p + 2) == Some(b">>") {
                                v.push((k, Obj::Null));
                                continue;
                            }
                            let val = self.value(depth + 1)?;
                            v.push((k, val));
                        }
                        _ => {}
                    }
                }
                Obj::Dict(v)
            }
            Tok::ArrClose | Tok::DictClose => Obj::Null,
        })
    }
}

// ---------- documentul ----------

struct Doc {
    objs: HashMap<u32, Obj>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// Datele unui flux, după filtre (FlateDecode, ASCIIHex, ASCII85; altele nu se citesc).
fn decode(dict: &[(Vec<u8>, Obj)], raw: &[u8]) -> Option<Vec<u8>> {
    let filters: Vec<Vec<u8>> = match dict.iter().find(|(k, _)| k == b"Filter").map(|(_, v)| v) {
        None => vec![],
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Arr(a)) => a.iter().filter_map(|x| x.name().map(<[u8]>::to_vec)).collect(),
        _ => return None,
    };
    let mut data = raw.to_vec();
    for f in filters {
        data = match &f[..] {
            b"FlateDecode" | b"Fl" => zlib_partial(&data, MAX_OUT),
            b"ASCIIHexDecode" | b"AHx" => {
                let mut s = vec![b'<'];
                s.extend_from_slice(&data);
                match Lexer::new(&s, 0).next() {
                    Some(Tok::Str(v)) => v,
                    _ => return None,
                }
            }
            b"ASCII85Decode" | b"A85" => ascii85(&data)?,
            _ => return None,
        };
    }
    Some(data)
}

fn ascii85(d: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut n) = (0u64, 0);
    for &c in d {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                acc = acc * 85 + u64::from(c - b'!');
                n += 1;
                if n == 5 {
                    out.extend_from_slice(&(acc as u32).to_be_bytes());
                    acc = 0;
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for _ in n..5 {
            acc = acc * 85 + 84;
        }
        out.extend_from_slice(&(acc as u32).to_be_bytes()[..n - 1]);
    }
    Some(out)
}

impl Doc {
    fn parse(d: &[u8]) -> Doc {
        let mut objs = HashMap::new();
        let mut from = 0;
        while let Some(i) = find(d, b"obj", from) {
            from = i + 3;
            // „N G obj”: cuvântul întreg, precedat de două numere.
            if d.get(i + 3).is_some_and(|&c| !is_ws(c) && !is_delim(c)) || i == 0 || !is_ws(d[i - 1]) {
                continue;
            }
            let mut j = i;
            while j > 0 && is_ws(d[j - 1]) {
                j -= 1;
            }
            while j > 0 && d[j - 1].is_ascii_digit() {
                j -= 1;
            }
            while j > 0 && is_ws(d[j - 1]) {
                j -= 1;
            }
            let ne = j;
            while j > 0 && d[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j == ne || (j > 0 && !is_ws(d[j - 1]) && !is_delim(d[j - 1])) {
                continue;
            }
            let Some(num) = std::str::from_utf8(&d[j..ne]).ok().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let mut lx = Lexer::new(d, i + 3);
            let Some(val) = lx.value(0) else { continue };
            let after = lx.p;
            let mut obj = val;
            if let Obj::Dict(dict) = obj.clone() {
                let mut l2 = Lexer::new(d, after);
                if l2.next() == Some(Tok::Kw(b"stream".to_vec())) {
                    let mut s = l2.p;
                    if d.get(s) == Some(&b'\r') {
                        s += 1;
                    }
                    if d.get(s) == Some(&b'\n') {
                        s += 1;
                    }
                    let direct = obj.get(b"Length").and_then(Obj::num).map(|n| n as usize);
                    let end = direct
                        .filter(|&n| {
                            let e = s.saturating_add(n);
                            let mut lx = Lexer::new(d, e.min(d.len()));
                            lx.next() == Some(Tok::Kw(b"endstream".to_vec()))
                        })
                        .map(|n| s + n)
                        .or_else(|| {
                            find(d, b"endstream", s).map(|mut e| {
                                while e > s && matches!(d[e - 1], b'\r' | b'\n') {
                                    e -= 1;
                                }
                                e
                            })
                        });
                    if let Some(e) = end {
                        obj = Obj::Stream(dict, d[s..e].to_vec());
                        from = e;
                    }
                }
            }
            // Actualizările incrementale vin mai târziu în fișier și câștigă.
            objs.insert(num, obj);
        }
        // Fluxuri de obiecte (PDF 1.5+).
        let streams: Vec<Obj> = objs
            .values()
            .filter(|o| matches!(o, Obj::Stream(..)) && o.get(b"Type").and_then(Obj::name) == Some(b"ObjStm"))
            .cloned()
            .collect();
        for st in streams {
            let Obj::Stream(dict, raw) = &st else { continue };
            let Some(data) = decode(dict, raw) else { continue };
            let n = st.get(b"N").and_then(Obj::num).unwrap_or(0.0) as usize;
            let first = st.get(b"First").and_then(Obj::num).unwrap_or(0.0) as usize;
            let mut lx = Lexer::new(&data, 0);
            let mut pairs = Vec::with_capacity(n.min(100_000));
            for _ in 0..n.min(100_000) {
                match (lx.next(), lx.next()) {
                    (Some(Tok::Num(a, _)), Some(Tok::Num(b, _))) => pairs.push((a as u32, b as usize)),
                    _ => break,
                }
            }
            for (num, off) in pairs {
                let mut lx = Lexer::new(&data, first.saturating_add(off));
                if let Some(v) = lx.value(0) {
                    objs.entry(num).or_insert(v);
                }
            }
        }
        Doc { objs }
    }

    fn resolve<'a>(&'a self, o: &'a Obj) -> &'a Obj {
        let mut cur = o;
        for _ in 0..16 {
            match cur {
                Obj::Ref(n) => cur = self.objs.get(n).unwrap_or(&Obj::Null),
                _ => return cur,
            }
        }
        &Obj::Null
    }

    fn get<'a>(&'a self, o: &'a Obj, k: &[u8]) -> Option<&'a Obj> {
        self.resolve(o).get(k).map(|v| self.resolve(v))
    }

    fn stream_data(&self, o: &Obj) -> Option<Vec<u8>> {
        match self.resolve(o) {
            Obj::Stream(d, raw) => decode(d, raw),
            _ => None,
        }
    }

    /// Paginile în ordine, fiecare cu resursele moștenite.
    fn pages(&self, d: &[u8]) -> Vec<(&Obj, Option<&Obj>)> {
        let root = d
            .windows(5)
            .rposition(|w| w == b"/Root")
            .and_then(|i| Lexer::new(d, i + 5).value(0))
            .map(|r| self.resolve(&r).clone())
            .filter(|c| c.get(b"Pages").is_some())
            .or_else(|| self.objs.values().find(|o| o.get(b"Type").and_then(Obj::name) == Some(b"Catalog")).cloned());
        let mut out = Vec::new();
        if let Some(root) = root {
            if let Some(Obj::Ref(n)) = root.get(b"Pages") {
                let mut seen = HashSet::new();
                self.walk(*n, None, &mut out, &mut seen, 0);
            }
        }
        if out.is_empty() {
            // Arbore stricat: paginile în ordinea din fișier.
            let mut v: Vec<(&u32, &Obj)> =
                self.objs.iter().filter(|(_, o)| o.get(b"Type").and_then(Obj::name) == Some(b"Page")).collect();
            v.sort_by_key(|(n, _)| **n);
            out = v.into_iter().map(|(_, o)| (o, None)).collect();
        }
        out
    }

    fn walk<'a>(&'a self, n: u32, res: Option<&'a Obj>, out: &mut Vec<(&'a Obj, Option<&'a Obj>)>, seen: &mut HashSet<u32>, depth: usize) {
        if depth > 64 || !seen.insert(n) || out.len() > 10_000 {
            return;
        }
        let Some(node) = self.objs.get(&n) else { return };
        let res = node.get(b"Resources").map(|r| self.resolve(r)).or(res);
        match node.get(b"Type").and_then(Obj::name) {
            Some(b"Page") => out.push((node, res)),
            _ => {
                if let Some(Obj::Arr(kids)) = node.get(b"Kids").map(|k| self.resolve(k)) {
                    for k in kids {
                        if let Obj::Ref(r) = k {
                            self.walk(*r, res, out, seen, depth + 1);
                        }
                    }
                }
            }
        }
    }
}

// ---------- fonturi ----------

struct Font {
    /// Cod → text (din `ToUnicode`).
    map: HashMap<u32, String>,
    /// Octeți pe cod (1 la fonturile simple, de obicei 2 la Type0).
    width: usize,
    /// Fără `ToUnicode` la un font compus: textul nu se poate afla.
    unknown: bool,
    diff: HashMap<u8, char>,
}

fn utf16be(b: &[u8]) -> String {
    let u: Vec<u16> = b.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
    String::from_utf16_lossy(&u)
}

fn be(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0, |a, &x| a << 8 | u32::from(x))
}

/// `beginbfchar` / `beginbfrange` dintr-un CMap ToUnicode.
fn parse_cmap(data: &[u8], map: &mut HashMap<u32, String>) -> Option<usize> {
    let mut lx = Lexer::new(data, 0);
    let mut width = None;
    let mut stack: Vec<Tok> = Vec::new();
    while let Some(t) = lx.next() {
        match &t {
            Tok::Kw(k) if k == b"endcodespacerange" => {
                if let Some(Tok::Str(s)) = stack.first() {
                    width = Some(s.len().clamp(1, 4));
                }
                stack.clear();
            }
            Tok::Kw(k) if k == b"endbfchar" => {
                for pair in stack.chunks(2) {
                    if let [Tok::Str(src), Tok::Str(dst)] = pair {
                        map.insert(be(src), utf16be(dst));
                    }
                }
                stack.clear();
            }
            Tok::Kw(k) if k == b"endbfrange" => {
                let mut i = 0;
                while i + 2 < stack.len() {
                    let (Some(Tok::Str(lo)), Some(Tok::Str(hi))) = (stack.get(i), stack.get(i + 1)) else { break };
                    let (lo, hi) = (be(lo), be(hi));
                    match stack.get(i + 2) {
                        Some(Tok::Str(dst)) => {
                            let base = utf16be(dst);
                            let mut chars: Vec<char> = base.chars().collect();
                            for (k, code) in (lo..=hi.min(lo.saturating_add(0xffff))).enumerate() {
                                if let Some(last) = chars.last_mut() {
                                    if k > 0 {
                                        *last = char::from_u32(*last as u32 + 1).unwrap_or(*last);
                                    }
                                }
                                map.insert(code, chars.iter().collect());
                            }
                            i += 3;
                        }
                        Some(Tok::ArrOpen) => {
                            let mut j = i + 3;
                            let mut code = lo;
                            while let Some(Tok::Str(dst)) = stack.get(j) {
                                if code <= hi {
                                    map.insert(code, utf16be(dst));
                                }
                                code = code.saturating_add(1);
                                j += 1;
                            }
                            i = j + 1;
                        }
                        _ => break,
                    }
                }
                stack.clear();
            }
            Tok::Kw(k) if k.starts_with(b"begin") => stack.clear(),
            _ => stack.push(t),
        }
    }
    width
}

/// Nume de glife frecvente din `/Differences` (restul: `uniXXXX` sau se ignoră).
fn glyph(name: &[u8]) -> Option<char> {
    if let Some(h) = name.strip_prefix(b"uni") {
        return std::str::from_utf8(h.get(..4)?).ok().and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
    }
    if name.len() == 1 && name[0].is_ascii_alphanumeric() {
        return Some(name[0] as char);
    }
    Some(match name {
        b"space" => ' ',
        b"period" => '.',
        b"comma" => ',',
        b"hyphen" | b"minus" => '-',
        b"colon" => ':',
        b"slash" => '/',
        b"zero" => '0',
        b"one" => '1',
        b"two" => '2',
        b"three" => '3',
        b"four" => '4',
        b"five" => '5',
        b"six" => '6',
        b"seven" => '7',
        b"eight" => '8',
        b"nine" => '9',
        b"abreve" => 'ă',
        b"Abreve" => 'Ă',
        b"acircumflex" => 'â',
        b"icircumflex" => 'î',
        b"scommaaccent" | b"scedilla" => 'ș',
        b"tcommaaccent" | b"tcedilla" => 'ț',
        b"Scommaaccent" | b"Scedilla" => 'Ș',
        b"Tcommaaccent" | b"Tcedilla" => 'Ț',
        _ => return None,
    })
}

impl Doc {
    fn font(&self, f: &Obj) -> Font {
        let mut map = HashMap::new();
        let type0 = self.get(f, b"Subtype").and_then(Obj::name) == Some(b"Type0");
        let mut width = if type0 { 2 } else { 1 };
        let mut has_map = false;
        if let Some(tu) = self.resolve(f).get(b"ToUnicode") {
            if let Some(data) = self.stream_data(tu) {
                if let Some(w) = parse_cmap(&data, &mut map) {
                    width = w;
                }
                has_map = !map.is_empty();
            }
        }
        let mut diff = HashMap::new();
        if let Some(Obj::Arr(a)) = self.get(f, b"Encoding").and_then(|e| e.get(b"Differences")).map(|x| self.resolve(x)) {
            let mut code = 0u32;
            for x in a {
                match x {
                    Obj::Num(n) => code = *n as u32,
                    Obj::Name(n) => {
                        if let (Ok(c), Some(ch)) = (u8::try_from(code), glyph(n)) {
                            diff.insert(c, ch);
                        }
                        code = code.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        Font { map, width, unknown: type0 && !has_map, diff }
    }
}

fn decode_str(s: &[u8], f: Option<&Font>, out: &mut String) {
    let Some(f) = f else {
        out.extend(s.iter().map(|&b| cp1252(b)));
        return;
    };
    if f.unknown {
        return;
    }
    for code in s.chunks(f.width) {
        let c = be(code);
        if let Some(t) = f.map.get(&c) {
            out.push_str(t);
        } else if f.width == 1 {
            let b = code[0];
            out.push(f.diff.get(&b).copied().unwrap_or_else(|| cp1252(b)));
        }
    }
}

fn cp1252(b: u8) -> char {
    if b < 0x80 {
        b as char
    } else {
        char::from_u32(u32::from(CP1252[usize::from(b - 0x80)])).unwrap_or('\u{fffd}')
    }
}

// ---------- conținutul paginilor ----------

struct Ctx<'a> {
    doc: &'a Doc,
    out: String,
    fonts: HashMap<Vec<u8>, Font>,
    max: usize,
}

impl Ctx<'_> {
    fn space(&mut self) {
        if !self.out.ends_with([' ', '\n']) && !self.out.is_empty() {
            self.out.push(' ');
        }
    }
    fn newline(&mut self) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.ends_with('\n') && !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    fn run(&mut self, data: &[u8], res: Option<&Obj>, depth: usize) {
        let doc = self.doc;
        let fonts_dict = res.and_then(|r| doc.get(r, b"Font"));
        let xobjects = res.and_then(|r| doc.get(r, b"XObject"));
        let mut cur_font: Option<Vec<u8>> = None;
        let mut ops: Vec<Tok> = Vec::new();
        let mut arr: Option<Vec<Tok>> = None;
        let mut lx = Lexer::new(data, 0);
        let mut last_y: Option<f64> = None;
        while let Some(t) = lx.next() {
            if self.out.len() > self.max {
                return;
            }
            match t {
                Tok::ArrOpen => arr = Some(Vec::new()),
                Tok::ArrClose => {
                    if let Some(a) = arr.take() {
                        ops.push(Tok::Kw(b"\0arr".to_vec()));
                        ops.extend(a);
                        ops.push(Tok::Kw(b"\0end".to_vec()));
                    }
                }
                Tok::DictOpen => {
                    // Dicționare inline (marcaje): se sar.
                    let mut depth = 1;
                    while depth > 0 {
                        match lx.next() {
                            Some(Tok::DictOpen) => depth += 1,
                            Some(Tok::DictClose) => depth -= 1,
                            None => break,
                            _ => {}
                        }
                    }
                }
                Tok::Kw(k) if arr.is_none() => {
                    match &k[..] {
                        b"Tf" => {
                            if let Some(Tok::Name(n)) = ops.iter().rev().find(|t| matches!(t, Tok::Name(_))) {
                                if !self.fonts.contains_key(n) {
                                    let f = fonts_dict.and_then(|fd| fd.get(n)).map(|f| doc.font(f));
                                    if let Some(f) = f {
                                        self.fonts.insert(n.clone(), f);
                                    }
                                }
                                cur_font = Some(n.clone());
                            }
                        }
                        b"Tj" | b"'" | b"\"" => {
                            if k != b"Tj" {
                                self.newline();
                            }
                            if let Some(Tok::Str(s)) = ops.last() {
                                let f = cur_font.as_ref().and_then(|n| self.fonts.get(n));
                                let mut tmp = String::new();
                                decode_str(s, f, &mut tmp);
                                self.out.push_str(&tmp);
                            }
                        }
                        b"TJ" => {
                            let f = cur_font.as_ref().and_then(|n| self.fonts.get(n));
                            let mut tmp = String::new();
                            for t in &ops {
                                match t {
                                    Tok::Str(s) => decode_str(s, f, &mut tmp),
                                    // Un spațiu mare între bucăți = spațiu între cuvinte.
                                    Tok::Num(n, _) if *n < -180.0 && !tmp.ends_with(' ') => tmp.push(' '),
                                    _ => {}
                                }
                            }
                            self.out.push_str(&tmp);
                        }
                        b"T*" => self.newline(),
                        b"Td" | b"TD" => {
                            let nums: Vec<f64> = ops.iter().filter_map(|t| if let Tok::Num(n, _) = t { Some(*n) } else { None }).collect();
                            if nums.len() >= 2 && nums[nums.len() - 1].abs() > 0.01 {
                                self.newline();
                            } else {
                                self.space();
                            }
                        }
                        b"Tm" => {
                            let nums: Vec<f64> = ops.iter().filter_map(|t| if let Tok::Num(n, _) = t { Some(*n) } else { None }).collect();
                            if nums.len() >= 6 {
                                let y = nums[nums.len() - 1];
                                match last_y {
                                    Some(ly) if (ly - y).abs() > 0.5 => self.newline(),
                                    _ => self.space(),
                                }
                                last_y = Some(y);
                            }
                        }
                        b"ET" => self.space(),
                        b"Do" if depth < 4 => {
                            if let Some(Tok::Name(n)) = ops.last() {
                                if let Some(x) = xobjects.and_then(|xo| doc.get(xo, n)) {
                                    if x.get(b"Subtype").and_then(Obj::name) == Some(b"Form") {
                                        if let Some(data) = doc.stream_data(x) {
                                            let r = x.get(b"Resources").map(|r| doc.resolve(r)).or(res);
                                            let saved = std::mem::take(&mut self.fonts);
                                            self.run(&data, r, depth + 1);
                                            self.fonts = saved;
                                        }
                                    }
                                }
                            }
                        }
                        b"BI" => {
                            // Imagine inline: datele binare până la „EI”.
                            if let Some(i) = find(data, b"ID", lx.p) {
                                let mut p = i + 2;
                                while let Some(e) = find(data, b"EI", p) {
                                    if data.get(e + 2).is_none_or(|&c| is_ws(c)) && e > 0 && is_ws(data[e - 1]) {
                                        lx.p = e + 2;
                                        break;
                                    }
                                    p = e + 2;
                                }
                            }
                        }
                        _ => {}
                    }
                    ops.clear();
                }
                other => match arr.as_mut() {
                    Some(a) => a.push(other),
                    None => {
                        if ops.len() > 64 {
                            ops.clear();
                        }
                        ops.push(other)
                    }
                },
            }
        }
    }
}

/// Textul unui PDF, sau `None` dacă e criptat ori nu se poate citi.
pub fn text(d: &[u8], max: usize) -> Option<String> {
    if !d.starts_with(b"%PDF") && find(&d[..d.len().min(1024)], b"%PDF", 0).is_none() {
        return None;
    }
    let doc = Doc::parse(d);
    if find(d, b"/Encrypt", 0).is_some()
        && doc.objs.values().any(|o| o.get(b"Encrypt").is_some() || o.get(b"Filter").and_then(Obj::name) == Some(b"Standard"))
    {
        return None;
    }
    let mut ctx = Ctx { doc: &doc, out: String::new(), fonts: HashMap::new(), max };
    for (page, res) in doc.pages(d) {
        let data: Vec<u8> = match page.get(b"Contents").map(|c| doc.resolve(c)) {
            Some(Obj::Arr(parts)) => {
                let mut v = Vec::new();
                for p in parts {
                    if let Some(x) = doc.stream_data(p) {
                        v.extend_from_slice(&x);
                        v.push(b'\n');
                    }
                }
                v
            }
            Some(c @ Obj::Stream(..)) => doc.stream_data(c).unwrap_or_default(),
            _ => continue,
        };
        ctx.fonts.clear();
        ctx.run(&data, res, 0);
        ctx.newline();
        ctx.out.push('\n');
        if ctx.out.len() > max {
            break;
        }
    }
    Some(ctx.out)
}
