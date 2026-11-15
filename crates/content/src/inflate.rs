//! Decompresie DEFLATE (RFC 1951) și zlib (RFC 1950), pentru ZIP (DOCX, XLSX, ODT) și PDF.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InflateError(pub &'static str);

/// Limita ieșirii: o arhivă-bombă nu poate umple memoria.
pub const MAX_OUT: usize = 64 << 20;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, bit: 0, nbits: 0 }
    }
    fn need(&mut self, n: u32) -> Result<(), InflateError> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or(InflateError("date trunchiate"))?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }
    fn bits(&mut self, n: u32) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align(&mut self) {
        self.bit = 0;
        self.nbits = 0;
    }
}

/// Tabel Huffman canonic: numărul de coduri pe lungime și simbolurile în ordine.
struct Huff {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Huff, InflateError> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &c in &counts[1..] {
            left = left * 2 - i32::from(c);
            if left < 0 {
                return Err(InflateError("cod Huffman invalid"));
            }
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { counts, symbols })
    }

    fn decode(&self, b: &mut Bits<'_>) -> Result<u16, InflateError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError("cod Huffman negăsit"))
    }
}

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits<'_>, out: &mut Vec<u8>, lit: &Huff, dist: &Huff, max: usize) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(b)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            _ => {
                let i = usize::from(sym - 257);
                if i >= 29 {
                    return Err(InflateError("lungime invalidă"));
                }
                let len = usize::from(LBASE[i]) + b.bits(u32::from(LEXT[i]))? as usize;
                let d = usize::from(dist.decode(b)?);
                if d >= 30 {
                    return Err(InflateError("distanță invalidă"));
                }
                let dist = usize::from(DBASE[d]) + b.bits(u32::from(DEXT[d]))? as usize;
                if dist > out.len() {
                    return Err(InflateError("distanță prea mare"));
                }
                let start = out.len() - dist;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
        }
        if out.len() > max {
            return Err(InflateError("ieșire prea mare"));
        }
    }
}

/// DEFLATE brut. `max` limitează ieșirea.
pub fn inflate(data: &[u8], max: usize) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::with_capacity(data.len().saturating_mul(3).min(max));
    inflate_into(data, max, &mut out)?;
    Ok(out)
}

/// Ce se poate decomprima dintr-un flux stricat sau trunchiat (des întâlnit în PDF-uri).
pub fn inflate_partial(data: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = inflate_into(data, max, &mut out);
    out
}

fn inflate_into(data: &[u8], max: usize, out: &mut Vec<u8>) -> Result<(), InflateError> {
    let mut b = Bits::new(data);
    loop {
        let last = b.bits(1)?;
        match b.bits(2)? {
            0 => {
                b.align();
                let p = b.pos;
                if p + 4 > data.len() {
                    return Err(InflateError("bloc necomprimat trunchiat"));
                }
                let len = usize::from(u16::from_le_bytes([data[p], data[p + 1]]));
                let nlen = u16::from_le_bytes([data[p + 2], data[p + 3]]);
                if nlen != !(len as u16) {
                    return Err(InflateError("lungime necomprimată greșită"));
                }
                let s = p + 4;
                let chunk = data.get(s..s + len).ok_or(InflateError("bloc necomprimat trunchiat"))?;
                out.extend_from_slice(chunk);
                b.pos = s + len;
                if out.len() > max {
                    return Err(InflateError("ieșire prea mare"));
                }
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huff::new(&l)?;
                let dist = Huff::new(&[5u8; 30])?;
                codes(&mut b, out, &lit, &dist, max)?;
            }
            2 => {
                let nlen = b.bits(5)? as usize + 257;
                let ndist = b.bits(5)? as usize + 1;
                let ncode = b.bits(4)? as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return Err(InflateError("antet dinamic invalid"));
                }
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(ncode) {
                    cl[o] = b.bits(3)? as u8;
                }
                let clh = Huff::new(&cl)?;
                let mut lens = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = clh.decode(&mut b)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => {
                            if i == 0 {
                                return Err(InflateError("repetare fără valoare"));
                            }
                            (lens[i - 1], 3 + b.bits(2)? as usize)
                        }
                        17 => (0, 3 + b.bits(3)? as usize),
                        _ => (0, 11 + b.bits(7)? as usize),
                    };
                    if i + rep > nlen + ndist {
                        return Err(InflateError("prea multe lungimi"));
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                }
                if lens[256] == 0 {
                    return Err(InflateError("lipsește sfârșitul de bloc"));
                }
                let lit = Huff::new(&lens[..nlen])?;
                let dist = Huff::new(&lens[nlen..])?;
                codes(&mut b, out, &lit, &dist, max)?;
            }
            _ => return Err(InflateError("tip de bloc invalid")),
        }
        if last == 1 {
            return Ok(());
        }
    }
}

/// zlib: antet de 2 octeți, apoi DEFLATE (suma de control de la final nu se verifică;
/// PDF-urile reale o au uneori greșită). Un flux stricat dă ce s-a putut decomprima.
pub fn zlib_partial(data: &[u8], max: usize) -> Vec<u8> {
    if data.len() >= 2 && data[0] & 0x0f == 8 && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0 {
        inflate_partial(&data[2..], max)
    } else {
        // Unele PDF-uri pun DEFLATE brut sub /FlateDecode.
        inflate_partial(data, max)
    }
}
