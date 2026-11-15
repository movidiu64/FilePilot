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
