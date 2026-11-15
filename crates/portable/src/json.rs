//! JSON minimal, fără dependențe: doar ce are nevoie formatul `.orgrules`.
//! Numerele sunt întregi (i64); un număr zecimal e respins.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub pos: usize,
    pub msg: &'static str,
}

const MAX_DEPTH: usize = 64;

pub fn parse(s: &str) -> Result<Value, ParseError> {
    let mut p = Parser { b: s.as_bytes(), s, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("text după valoare"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> ParseError {
        ParseError { pos: self.i, msg }
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), ParseError> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err("caracter neașteptat"))
        }
    }

    fn lit(&mut self, w: &str, v: Value) -> Result<Value, ParseError> {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("valoare necunoscută"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("imbricare prea adâncă"));
        }
        match self.b.get(self.i) {
            Some(b'n') => self.lit("null", Value::Null),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(v));
                }
                loop {
                    self.ws();
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(v));
                        }
                        _ => return Err(self.err("se aștepta , sau ]")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    v.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(v));
                        }
                        _ => return Err(self.err("se aștepta , sau }")),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let st = self.i;
                self.i += 1;
                while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
                if matches!(self.b.get(self.i), Some(b'.' | b'e' | b'E')) {
                    return Err(self.err("numerele zecimale nu sunt acceptate"));
                }
                self.s[st..self.i].parse().map(Value::Int).map_err(|_| self.err("număr invalid"))
            }
            _ => Err(self.err("valoare așteptată")),
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let h = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("escape \\u incomplet"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("escape \\u invalid"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let rest = &self.s[self.i..];
            let Some(c) = rest.chars().next() else { return Err(self.err("șir neînchis")) };
            self.i += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.b.get(self.i).copied().ok_or_else(|| self.err("escape incomplet"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if !self.s[self.i..].starts_with("\\u") {
                                    return Err(self.err("surogat singur"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("surogat invalid"));
                                }
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            out.push(char::from_u32(cp).ok_or_else(|| self.err("caracter invalid"))?);
                        }
                        _ => return Err(self.err("escape necunoscut")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.err("caracter de control în șir")),
                c => out.push(c),
            }
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Serializare cu indentare, ușor de citit și de comparat.
pub fn to_string_pretty(v: &Value) -> String {
    let mut out = String::new();
    write(&mut out, v, 0);
    out.push('\n');
    out
}

fn write(out: &mut String, v: &Value, ind: usize) {
    let pad = |o: &mut String, n: usize| o.push_str(&"  ".repeat(n));
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Str(s) => write_str(out, s),
        Value::Arr(a) if a.is_empty() => out.push_str("[]"),
        Value::Arr(a) => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                pad(out, ind + 1);
                write(out, x, ind + 1);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            pad(out, ind);
            out.push(']');
        }
        Value::Obj(o) if o.is_empty() => out.push_str("{}"),
        Value::Obj(o) => {
            out.push_str("{\n");
            for (i, (k, x)) in o.iter().enumerate() {
                pad(out, ind + 1);
                write_str(out, k);
                out.push_str(": ");
                write(out, x, ind + 1);
                out.push_str(if i + 1 < o.len() { ",\n" } else { "\n" });
            }
            pad(out, ind);
            out.push('}');
        }
    }
}
