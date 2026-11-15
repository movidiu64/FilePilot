use std::io;

use org_fs::Fs;
use org_sysfs::SysFs;

/// Atinge Coșul real: trimite un fișier nou acolo și îl readuce.
#[test]
fn trash_and_restore_on_the_real_recycle_bin() {
    let dir = std::env::temp_dir().join(format!("org-sysfs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("proba-{}.txt", std::process::id()));
    std::fs::write(&file, b"continut").unwrap();

    let mut fs = SysFs;
    fs.trash(&file).unwrap();
    assert!(!file.exists(), "fișierul a plecat în Coș");

    if let Err(e) = fs.restore_from_trash(&file) {
        // Diagnostic pentru CI: ce vede Coșul pentru fișierele noastre.
        for i in trash::os_limited::list().unwrap_or_default() {
            if i.name.to_string_lossy().contains("proba-") {
                eprintln!("în Coș: {:?} din {:?}", i.name, i.original_parent);
            }
        }
        panic!("restaurare: {e} (căutat în {:?}, canonic {:?})", file.parent(), std::fs::canonicalize(&dir));
    }
    // Shell-ul poate termina restaurarea puțin după ce apelul a întors.
    for _ in 0..50 {
        if file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !file.exists() {
        let here: Vec<_> = std::fs::read_dir(&dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        let bin: Vec<_> = trash::os_limited::list()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.name.to_string_lossy().contains("proba-"))
            .map(|i| (i.name, i.original_parent))
            .collect();
        panic!("după restaurare lipsește {file:?}; în folder: {here:?}; în Coș: {bin:?}");
    }
    assert_eq!(std::fs::read(&file).unwrap(), b"continut", "readus intact");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restoring_something_that_is_not_in_the_bin_is_not_found() {
    let mut fs = SysFs;
    let e = fs
        .restore_from_trash(std::path::Path::new("C:\\org-nu-exista-niciodata\\x-98127361.dat"))
        .unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

#[test]
fn everything_else_behaves_like_std_fs() {
    let dir = std::env::temp_dir().join(format!("org-sysfs-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut fs = SysFs;
    fs.create_dir_all(&dir.join("a")).unwrap();
    std::fs::write(dir.join("a/x.txt"), b"12345").unwrap();
    assert_eq!(fs.metadata(&dir.join("a/x.txt")).unwrap().size, 5);
    assert_eq!(fs.list_dir(&dir.join("a")).unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
