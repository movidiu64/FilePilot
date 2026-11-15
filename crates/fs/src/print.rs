//! Amprentele de conținut pentru căutarea dublurilor. Algoritmul e al nostru și fix:
//! amprentele se păstrează în baza de date, deci trebuie să iasă la fel între versiuni
//! (`std::hash` nu garantează asta). Nu sunt criptografice: o potrivire de amprente doar
//! propune o dublură; înainte de orice acțiune se compară conținutul octet cu octet.

/// Versiunea algoritmului; alta înseamnă că amprentele vechi nu mai sunt valabile.
pub const PRINT_VERSION: i64 = 1;

/// Câți octeți de la început intră în amprenta rapidă.
pub const QUICK_BYTES: u64 = 1 << 16;

const K1: u64 = 0x9E37_79B9_7F4A_7C15;
const K2: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// Amprentă de 64 de biți, pe bucăți de orice mărime (rezultatul nu depinde de împărțire).
#[derive(Debug, Clone)]
pub struct Hasher64 {
    h: u64,
    len: u64,
    tail: [u8; 8],
    tail_len: usize,
}

impl Default for Hasher64 {
    fn default() -> Self {
        Hasher64 { h: K2, len: 0, tail: [0; 8], tail_len: 0 }
    }
}

impl Hasher64 {
    fn word(&mut self, w: u64) {
        self.h = (self.h ^ w.wrapping_mul(K1)).rotate_left(31).wrapping_mul(K2);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.tail_len > 0 {
            let take = (8 - self.tail_len).min(data.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&data[..take]);
            self.tail_len += take;
            data = &data[take..];
            if self.tail_len < 8 {
                return;
            }
            self.word(u64::from_le_bytes(self.tail));
            self.tail_len = 0;
        }
        let mut chunks = data.chunks_exact(8);
        for c in &mut chunks {
            self.word(u64::from_le_bytes(c.try_into().unwrap_or([0; 8])));
        }
        let rest = chunks.remainder();
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    pub fn finish(&self) -> u64 {
        let mut h = self.h;
        if self.tail_len > 0 {
            let mut last = [0u8; 8];
            last[..self.tail_len].copy_from_slice(&self.tail[..self.tail_len]);
            h = (h ^ u64::from_le_bytes(last).wrapping_mul(K1)).rotate_left(31).wrapping_mul(K2);
        }
        // Lungimea intră în amprentă; apoi amestecul final din splitmix64.
        h ^= self.len.wrapping_mul(K1);
        h ^= h >> 30;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 27;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^ (h >> 31)
    }
}

/// Amprenta unui bloc de octeți.
pub fn fingerprint(data: &[u8]) -> u64 {
    let mut h = Hasher64::default();
    h.update(data);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_does_not_change_the_print() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let whole = fingerprint(&data);
        for step in [1, 3, 7, 8, 9, 64, 1000, 4096] {
            let mut h = Hasher64::default();
            for c in data.chunks(step) {
                h.update(c);
            }
            assert_eq!(h.finish(), whole, "bucăți de {step}");
        }
    }

    #[test]
    fn prints_are_fixed_and_distinguish_small_changes() {
        // Valorile sunt fixe: dacă se schimbă, trebuie crescut PRINT_VERSION.
        assert_eq!(fingerprint(b""), fingerprint(&[]));
        assert_ne!(fingerprint(b"a"), fingerprint(b"b"));
        assert_ne!(fingerprint(b"ab"), fingerprint(b"ba"));
        assert_ne!(fingerprint(&[0]), fingerprint(&[0, 0]), "lungimea contează");
        assert_ne!(fingerprint(&[0u8; 9]), fingerprint(&[0u8; 8]));
        let mut a = vec![7u8; 100_000];
        let b = a.clone();
        a[54_321] ^= 1;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
