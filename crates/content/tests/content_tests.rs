use org_content::inflate::{inflate, zlib_partial, MAX_OUT};
use org_content::{extract, plain, read_limit};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn inflate_fixed_dynamic_and_stored_blocks() {
    // zlib.compress(b"hello hello hello", 9): Huffman fix.
    assert_eq!(zlib_partial(&hex("78dacb48cdc9c957c84090003a2e067d"), MAX_OUT), b"hello hello hello");
    // Huffman dinamic, cu copieri lungi și diacritice.
    let want = "Șurubul M8 are tensiunea de rupere 800 MPa. ".repeat(40);
    let z = hex("78da3b31a3b4a834a93447c1d74221b12855a12435af38b3342f3551212555a1a8b4201528666160a0e01b90a8a7706254f1a8e251c5a38a47158f2a1e5e8a0159a7725f");
    assert_eq!(String::from_utf8(zlib_partial(&z, MAX_OUT)).unwrap(), want);
    // Bloc necomprimat: BFINAL=1, BTYPE=00, LEN=3, NLEN=!3.
    assert_eq!(inflate(&[0x01, 3, 0, 0xfc, 0xff, b'a', b'b', b'c'], MAX_OUT).unwrap(), b"abc");
    // Limita ieșirii oprește o „bombă”.
    assert!(inflate(&z[2..], 100).is_err());
    // Trunchiat: ce s-a putut decomprima, fără panică.
    assert!(want.starts_with(std::str::from_utf8(&zlib_partial(&z[..20], MAX_OUT)).unwrap_or("")));
}

#[test]
fn office_documents() {
    let docx = extract("docx", &fixture("factura.docx")).unwrap();
    for needle in ["Factură fiscală nr. 2025-0042", "Țesătoria Ștefănești", "& „ghilimele” <tag>", "Ax canelat DIN 5480", "1.469,13 lei"] {
        assert!(docx.contains(needle), "{needle} lipsește din:\n{docx}");
    }
    // Tab-ul și paragrafele rămân separatoare.
    assert!(docx.contains("12.11.2025\tScadență"));
    assert!(docx.contains("Produs\nPreț"), "{docx}");
    let odt = extract("odt", &fixture("factura.odt")).unwrap();
    assert!(odt.contains("Total de plată: 1.469,13 lei") && odt.contains("încă două"));
    let xlsx = extract("xlsx", &fixture("stoc.xlsx")).unwrap();
    assert!(xlsx.contains("Comandă") && xlsx.contains("Rulment 6205") && xlsx.contains("84"), "{xlsx}");
}

#[test]
fn pdf_text_simple_fonts_and_identity_h_with_object_streams() {
    let s = extract("pdf", &fixture("simplu.pdf")).unwrap();
    assert!(s.contains("Invoice number INV-7781\nTotal due: 99.50 EUR"), "{s}");
    assert!(s.contains("Page two text"));
    // LibreOffice: fonturi Type0/Identity-H cu ToUnicode, obiecte în fluxuri comprimate (qpdf).
    let f = extract("pdf", &fixture("factura-objstm.pdf")).unwrap();
    for needle in ["Antet: SC Exemplu SRL", "Factură fiscală nr. 2025-0042", "Țesătoria Ștefănești — șase pături", "1.234,56 lei"] {
        assert!(f.contains(needle), "{needle} lipsește din:\n{f}");
    }
    // Criptat (AES-256, fără parolă de deschidere): nu se încearcă.
    assert_eq!(extract("pdf", &fixture("criptat.pdf")), None);
}

#[test]
fn plain_text_encodings() {
    assert_eq!(extract("txt", &fixture("cp1250.txt")).unwrap(), "Ofertă pentru şurub M8 — preţ mic");
    assert_eq!(extract("log", &fixture("utf16.txt")).unwrap(), "Raport lunar: producție");
    assert_eq!(plain("ăîâșț".as_bytes()).unwrap(), "ăîâșț");
    // Tăiat în mijlocul unui caracter UTF-8 (limita de citire): rămâne UTF-8.
    let b = "preț".as_bytes();
    assert_eq!(plain(&b[..b.len() - 1]).unwrap(), "pre");
    // Binar fără BOM.
    assert_eq!(plain(&[b'M', b'Z', 0, 0, 3]), None);
    assert_eq!(extract("jpg", b"text"), None);
    assert_eq!(read_limit("png", 10), None);
    assert_eq!(read_limit("pdf", 1000), Some(1000));
    assert_eq!(read_limit("pdf", 1 << 40), None);
    assert_eq!(read_limit("txt", 1 << 40), Some(org_content::MAX_PLAIN));
}

#[test]
fn damaged_files_never_panic() {
    // Variații deterministe ale fișierelor reale: octeți schimbați, trunchieri.
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (ext, name) in [("docx", "factura.docx"), ("pdf", "factura-objstm.pdf"), ("pdf", "simplu.pdf"), ("xlsx", "stoc.xlsx"), ("odt", "factura.odt")] {
        let orig = fixture(name);
        for i in 0..300 {
            let mut d = orig.clone();
            if i % 3 == 0 {
                d.truncate((rnd() as usize) % d.len());
            } else {
                for _ in 0..1 + rnd() % 20 {
                    let p = (rnd() as usize) % d.len();
                    d[p] = rnd() as u8;
                }
            }
            let _ = extract(ext, &d);
        }
    }
    for n in 0..200 {
        let d: Vec<u8> = (0..n * 7).map(|_| rnd() as u8).collect();
        let _ = inflate(&d, 1 << 16);
        let _ = extract("pdf", &[b"%PDF-1.7\n".as_slice(), &d].concat());
    }
}
