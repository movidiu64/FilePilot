use std::io;
use std::path::{Path, PathBuf};

use org_fs::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn unique_path_numbers_like_the_spec() {
    let mut fs = MemFs::new();
    fs.add_file("/d/invoice.docx", 1, 0);
    assert_eq!(unique_path(&fs, &p("/d/invoice.docx")), p("/d/invoice_2.docx"));
    fs.add_file("/d/invoice_2.docx", 1, 0);
    assert_eq!(unique_path(&fs, &p("/d/invoice.docx")), p("/d/invoice_3.docx"));
    assert_eq!(unique_path(&fs, &p("/d/free.txt")), p("/d/free.txt"));
    // fără extensie
    fs.add_file("/d/README", 1, 0);
    assert_eq!(unique_path(&fs, &p("/d/README")), p("/d/README_2"));
}

#[test]
fn same_volume_move_is_a_rename() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.add_dir("/b");
    move_file(&mut fs, Path::new("/a/x.pdf"), Path::new("/b/x.pdf")).unwrap();
    assert!(fs.stat("/a/x.pdf").is_none());
    assert_eq!(fs.stat("/b/x.pdf"), Some(FileStat { size: 10, modified: 5 }));
}

#[test]
fn cross_device_move_copies_verifies_then_removes() {
    let mut fs = MemFs::new();
    fs.set_volumes(&["/c", "/d"]);
    fs.add_file("/c/x.pdf", 10, 5);
    fs.add_dir("/d/out");
    move_file(&mut fs, Path::new("/c/x.pdf"), Path::new("/d/out/x.pdf")).unwrap();
    assert!(fs.stat("/c/x.pdf").is_none());
    assert_eq!(fs.stat("/d/out/x.pdf").unwrap().size, 10);
}

#[test]
fn torn_copy_across_devices_leaves_source_intact_and_cleans_up() {
    let mut fs = MemFs::new();
    fs.set_volumes(&["/c", "/d"]);
    fs.add_file("/c/x.pdf", 10, 5);
    fs.add_dir("/d/out");
    fs.torn_next_copy();
    let err = move_file(&mut fs, Path::new("/c/x.pdf"), Path::new("/d/out/x.pdf")).unwrap_err();
    assert!(err.to_string().contains("torn"));
    assert_eq!(fs.stat("/c/x.pdf").unwrap().size, 10, "sursa rămâne intactă");
    assert!(fs.stat("/d/out/x.pdf").is_none(), "copia parțială se șterge");
}

#[test]
fn move_into_missing_folder_fails_like_a_real_fs() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    let err = move_file(&mut fs, Path::new("/a/x.pdf"), Path::new("/nu_exista/x.pdf")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(fs.stat("/a/x.pdf").is_some());
}

#[test]
fn fail_at_hits_exactly_the_chosen_operation_once() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.add_dir("/b");
    fs.fail_at(1);
    fs.copy(Path::new("/a/x.pdf"), Path::new("/b/1.pdf")).unwrap(); // op 0
    assert!(fs.copy(Path::new("/a/x.pdf"), Path::new("/b/2.pdf")).is_err()); // op 1
    fs.copy(Path::new("/a/x.pdf"), Path::new("/b/3.pdf")).unwrap(); // op 2: eșecul s-a consumat
    assert_eq!(fs.ops(), 3);
}

#[test]
fn remove_empty_parents_stops_at_root_and_at_non_empty_dirs() {
    let mut fs = MemFs::new();
    fs.add_dir("/dl/a/b/c");
    remove_empty_parents(&mut fs, Path::new("/dl/a/b/c"), Path::new("/dl"));
    assert!(!fs.has_dir("/dl/a"), "toate folderele goale dispar");
    assert!(fs.has_dir("/dl"), "folderul monitorizat rămâne");

    let mut fs = MemFs::new();
    fs.add_dir("/dl/a/b/c");
    fs.add_file("/dl/a/keep.txt", 1, 0);
    remove_empty_parents(&mut fs, Path::new("/dl/a/b/c"), Path::new("/dl"));
    assert!(!fs.has_dir("/dl/a/b"));
    assert!(fs.has_dir("/dl/a"), "un folder cu fișiere rămâne");
}

#[test]
fn trash_and_restore() {
    let mut fs = MemFs::new();
    fs.add_file("/a/x.pdf", 10, 5);
    fs.trash(Path::new("/a/x.pdf")).unwrap();
    assert!(fs.stat("/a/x.pdf").is_none());
    assert_eq!(fs.trashed(), vec![p("/a/x.pdf")]);
    fs.restore_from_trash(Path::new("/a/x.pdf")).unwrap();
    assert!(fs.stat("/a/x.pdf").is_some());
    assert!(fs.restore_from_trash(Path::new("/a/x.pdf")).is_err());
}

#[test]
fn std_fs_works_on_a_real_temp_dir() {
    let base = std::env::temp_dir().join(format!("org-fs-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("src")).unwrap();
    std::fs::write(base.join("src/a.txt"), b"hello").unwrap();

    let mut fs = StdFs;
    fs.create_dir_all(&base.join("dst/deep")).unwrap();
    move_file(&mut fs, &base.join("src/a.txt"), &base.join("dst/deep/a.txt")).unwrap();
    assert!(!fs.exists(&base.join("src/a.txt")));
    assert_eq!(fs.metadata(&base.join("dst/deep/a.txt")).unwrap().size, 5);

    let copied = fs.copy(&base.join("dst/deep/a.txt"), &base.join("copy.txt")).unwrap();
    assert_eq!(copied, 5);
    assert_eq!(
        fs.metadata(&base.join("copy.txt")).unwrap().modified,
        fs.metadata(&base.join("dst/deep/a.txt")).unwrap().modified,
        "copia păstrează data modificării"
    );
    assert_eq!(unique_path(&fs, &base.join("copy.txt")), base.join("copy_2.txt"));

    // Folderele goale se șterg, cele pline nu.
    assert!(!fs.remove_dir_if_empty(&base.join("dst")).unwrap());
    fs.remove_file(&base.join("dst/deep/a.txt")).unwrap();
    remove_empty_parents(&mut fs, &base.join("dst/deep"), &base);
    assert!(!fs.exists(&base.join("dst")));
    assert_eq!(fs.trash(&base.join("copy.txt")).unwrap_err().kind(), io::ErrorKind::Unsupported);
    let _ = std::fs::remove_dir_all(&base);
}

/// Comanda reală: codul de ieșire și mesajul de eroare ajung în jurnal.
#[cfg(unix)]
#[test]
fn run_reports_exit_code_and_stderr() {
    use org_fs::{Fs, StdFs};
    let mut fs = StdFs;
    assert_eq!(fs.run("sh", &["-c".into(), "exit 0".into()]).unwrap(), 0);
    let e = fs.run("sh", &["-c".into(), "echo nu gasesc fisierul >&2; exit 3".into()]).unwrap_err();
    assert_eq!(e.to_string(), "cod de ieșire 3: nu gasesc fisierul");
    assert!(fs.run("program-care-nu-exista-xyz", &[]).is_err());
}

/// Previzualizarea nu atinge discul: mutarea, copia, Coșul și comenzile rămân în memorie,
/// iar conflictele se văd între fișierele previzualizate (al doilea primește `_2`).
#[test]
fn dry_fs_simulates_without_touching_the_real_one() {
    use org_fs::{move_file, unique_path, DryFs, Fs, MemFs};
    use std::path::Path;
    let mut real = MemFs::new();
    real.add_file("/dl/a.pdf", 10, 1);
    real.add_file("/dl/b.pdf", 20, 2);
    real.add_file("/pdf/a.pdf", 5, 1);
    let before = real.file_paths();
    {
        let mut dry = DryFs::new(&real);
        dry.create_dir_all(Path::new("/nou")).unwrap();
        assert!(dry.exists(Path::new("/nou")));
        let t = unique_path(&dry, Path::new("/pdf/a.pdf"));
        assert_eq!(t, Path::new("/pdf/a_2.pdf"));
        move_file(&mut dry, Path::new("/dl/a.pdf"), &t).unwrap();
        assert!(!dry.exists(Path::new("/dl/a.pdf")));
        assert_eq!(dry.metadata(&t).unwrap().size, 10);
        // următorul fișier cu același nume vede ținta ocupată
        assert_eq!(unique_path(&dry, Path::new("/pdf/a.pdf")), Path::new("/pdf/a_3.pdf"));
        dry.copy(Path::new("/dl/b.pdf"), Path::new("/bk/b.pdf")).unwrap();
        dry.trash(Path::new("/dl/b.pdf")).unwrap();
        assert!(dry.trash(Path::new("/dl/b.pdf")).is_err());
        assert_eq!(dry.run("format", &["c:".into()]).unwrap(), 0);
        let listed: Vec<_> = dry.list_dir(Path::new("/pdf")).unwrap().into_iter().map(|e| e.path).collect();
        assert!(listed.contains(&Path::new("/pdf/a_2.pdf").to_path_buf()));
    }
    assert_eq!(real.file_paths(), before, "discul real a rămas neatins");
    assert!(real.ran.is_empty(), "nicio comandă rulată");
}

/// Pe Windows: un fișier ținut deschis de alt program (aici, de noi) e „în uz”.
#[cfg(windows)]
#[test]
fn open_file_is_in_use_on_windows() {
    use std::io::Write;
    let p = std::env::temp_dir().join(format!("org-in-use-{}.bin", std::process::id()));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(b"partial").unwrap();
    assert!(StdFs.in_use(&p), "scris încă: în uz");
    drop(f);
    assert!(!StdFs.in_use(&p), "închis: liber");
    std::fs::remove_file(&p).unwrap();
}

#[test]
fn hash_file_on_disk_matches_the_print_of_the_content() {
    let dir = std::env::temp_dir().join(format!("org-fs-hash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Peste o bucată de citire (1 MB) și nealiniat la 8 octeți.
    let data: Vec<u8> = (0..(3 << 20) + 13u32).map(|i| (i % 251) as u8).collect();
    let p = dir.join("mare.bin");
    std::fs::write(&p, &data).unwrap();
    assert_eq!(StdFs.hash_file(&p).unwrap(), fingerprint(&data));
    let mut m = MemFs::new();
    m.add_file_with("/x.bin", &data, 0);
    assert_eq!(m.hash_file(Path::new("/x.bin")).unwrap(), fingerprint(&data));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- archives with the system tar ----------

/// A minimal ustar archive: (name, content) per file.
fn tar_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in files {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(b"14000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("org-unpack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn std_fs_unpacks_a_tar_archive() {
    let base = temp_dir("tar");
    let archive = base.join("photos.tar");
    std::fs::write(&archive, tar_bytes(&[("a.txt", b"hello"), ("sub/b.txt", b"world!")])).unwrap();
    let into = base.join("out");
    std::fs::create_dir(&into).unwrap();
    let mut fs = StdFs;
    match fs.unpack(&archive, &into) {
        Err(e) if e.kind() == io::ErrorKind::Unsupported => return, // no tar on this system
        r => r.unwrap(),
    }
    assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(into.join("sub").join("b.txt")).unwrap(), b"world!");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn std_fs_never_writes_outside_the_target() {
    let base = temp_dir("escape");
    let archive = base.join("evil.tar");
    std::fs::write(&archive, tar_bytes(&[("../escaped.txt", b"x"), ("ok.txt", b"y")])).unwrap();
    let into = base.join("out");
    std::fs::create_dir(&into).unwrap();
    // bsdtar (Windows) refuses the archive; GNU tar strips the `../`. Either way nothing
    // lands next to the target folder.
    let _ = StdFs.unpack(&archive, &into);
    assert!(!base.join("escaped.txt").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn std_fs_reports_a_damaged_archive() {
    let base = temp_dir("broken");
    let archive = base.join("broken.zip");
    std::fs::write(&archive, b"this is not an archive at all").unwrap();
    let into = base.join("out");
    std::fs::create_dir(&into).unwrap();
    let r = StdFs.unpack(&archive, &into);
    assert!(r.is_err());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(windows)]
fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// A ZIP with stored (uncompressed) entries, as Explorer and browsers produce them.
#[cfg(windows)]
fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let (crc, len, off) = (crc32(data), data.len() as u32, out.len() as u32);
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&20u16.to_le_bytes()); // version needed
            v.extend_from_slice(&0u16.to_le_bytes()); // flags
            v.extend_from_slice(&0u16.to_le_bytes()); // stored
            v.extend_from_slice(&0u16.to_le_bytes()); // time
            v.extend_from_slice(&0x5b6fu16.to_le_bytes()); // date 2025-11-15
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes()); // extra
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        common(&mut central);
        central.extend_from_slice(&[0u8; 8]); // comment, disk, internal and external attributes
        central.extend_from_slice(&[0u8; 2]);
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let (cd_off, cd_len, n) = (out.len() as u32, central.len() as u32, files.len() as u16);
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(windows)]
#[test]
fn std_fs_unpacks_a_zip_with_the_windows_tar() {
    let base = temp_dir("zip");
    let archive = base.join("photos.zip");
    std::fs::write(&archive, zip_bytes(&[("a.txt", b"hello"), ("sub/b.txt", b"world!")])).unwrap();
    let into = base.join("out");
    std::fs::create_dir(&into).unwrap();
    StdFs.unpack(&archive, &into).unwrap();
    assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(into.join("sub").join("b.txt")).unwrap(), b"world!");

    let evil = base.join("evil.zip");
    std::fs::write(&evil, zip_bytes(&[("../escaped.txt", b"x")])).unwrap();
    let into2 = base.join("out2");
    std::fs::create_dir(&into2).unwrap();
    assert!(StdFs.unpack(&evil, &into2).is_err(), "a path with .. must fail the extraction");
    assert!(!base.join("escaped.txt").exists());
    let _ = std::fs::remove_dir_all(&base);
}
