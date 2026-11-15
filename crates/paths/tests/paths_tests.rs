use org_paths::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("org-paths-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn only_one_instance_holds_the_lock_and_it_is_released_on_drop() {
    let d = tmp("lock");
    let f = DataFiles::in_dir(&d);
    assert!(!daemon_running(&f.lock));
    let first = acquire_lock(&f.lock).unwrap().expect("primul ia lacătul");
    assert!(daemon_running(&f.lock));
    assert!(acquire_lock(&f.lock).unwrap().is_none(), "al doilea e refuzat");
    drop(first);
    assert!(!daemon_running(&f.lock));
    assert!(acquire_lock(&f.lock).unwrap().is_some());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn files_live_under_the_data_dir() {
    let f = DataFiles::in_dir(std::path::Path::new("/x"));
    assert_eq!(f.rules, std::path::Path::new("/x").join("rules.orgrules"));
    assert_eq!(f.db, std::path::Path::new("/x").join("organizator.db"));
}

#[test]
fn requests_are_written_atomically_and_listed_in_order() {
    let dir = std::env::temp_dir().join(format!("orgreq{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let a = org_paths::write_request(&dir, "{}").unwrap();
    let b = org_paths::write_request(&dir, "{}").unwrap();
    std::fs::write(dir.join("request-x.tmp"), "pe jumătate").unwrap();
    std::fs::write(dir.join("alt.json"), "{}").unwrap();
    assert!(org_paths::is_request_file(&a) && !org_paths::is_request_file(&dir.join("request-x.tmp")));
    assert_eq!(org_paths::pending_requests(&dir), vec![a, b]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rule_backups_are_spaced_and_pruned() {
    use std::time::Duration;
    let d = tmp("backup");
    let rules = d.join(RULES_FILE);
    // Fără fișier de reguli nu e nimic de copiat.
    assert!(backup_rules(&d, &rules, "2026-01-01_10-00-00", Duration::ZERO, 3).unwrap().is_none());
    std::fs::write(&rules, "v1").unwrap();
    let b1 = backup_rules(&d, &rules, "2026-01-01_10-00-00", Duration::from_secs(600), 3).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&b1).unwrap(), "v1");
    // Copia de acum câteva clipe ajunge pentru aceeași sesiune.
    assert!(backup_rules(&d, &rules, "2026-01-01_10-01-00", Duration::from_secs(600), 3).unwrap().is_none());
    for (i, st) in ["2026-01-02_10-00-00", "2026-01-03_10-00-00", "2026-01-03_10-00-00", "2026-01-04_10-00-00"].iter().enumerate() {
        std::fs::write(&rules, format!("v{}", i + 2)).unwrap();
        assert!(backup_rules(&d, &rules, st, Duration::ZERO, 3).unwrap().is_some());
    }
    let names: Vec<String> =
        rule_backups(&d).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["rules-2026-01-03_10-00-00.orgrules", "rules-2026-01-03_10-00-00_2.orgrules", "rules-2026-01-04_10-00-00.orgrules"]);
}

#[test]
fn the_former_data_folder_is_moved_once() {
    let base = std::env::temp_dir().join(format!("fp-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("Organizator")).unwrap();
    std::fs::write(base.join("Organizator").join("rules.orgrules"), "x").unwrap();
    let d = org_paths::migrate_legacy_dir(&base);
    assert_eq!(d, base.join("FilePilot"));
    assert_eq!(std::fs::read_to_string(d.join("rules.orgrules")).unwrap(), "x");
    assert!(!base.join("Organizator").exists());
    // Both exist (e.g. restored by hand): the new one wins and nothing is touched.
    std::fs::create_dir_all(base.join("Organizator")).unwrap();
    assert_eq!(org_paths::migrate_legacy_dir(&base), base.join("FilePilot"));
    assert!(base.join("Organizator").exists());
    // Neither: the new name.
    let empty = base.join("empty");
    assert_eq!(org_paths::migrate_legacy_dir(&empty), empty.join("FilePilot"));
    let _ = std::fs::remove_dir_all(&base);
}
