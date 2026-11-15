//! The "Extract archive" action, on the in-memory file system. Test archives are text:
//! one `path=content` line per file (see `MemFs::unpack`).

use std::collections::HashMap;
use std::path::PathBuf;

use org_actions::*;
use org_core::timefmt::{timestamp, Lang};
use org_core::*;
use org_fs::{DryFs, Fs, MemFs};
use org_journal::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn t(s: &str) -> Template {
    Template::parse(s).unwrap()
}

fn rule(actions: Vec<Action>) -> Rule {
    Rule {
        id: 1,
        name: "unzip".into(),
        enabled: true,
        hidden: false,
        folders: vec![WatchedFolder { path: p("/dl"), recursive: true }],
        condition: Condition::AllFiles,
        actions,
        continue_after_move: true,
        simulate: false,
    }
}

fn extract(dest: &str, conflict: Conflict) -> Action {
    Action::Extract { dest: t(dest), conflict }
}

const NOW: (i64, u32, u32) = (2026, 11, 15);

fn ctx(d: &HashMap<String, Dictionary>) -> RenderCtx<'_> {
    RenderCtx { now: timestamp(NOW.0, NOW.1, NOW.2, 12, 0, 0), tz: Tz::Fixed(0), lang: Lang::En, dicts: d, content: None, raw: None }
}

fn meta(fs: &MemFs, path: &str) -> FileMeta {
    let st = fs.stat(path).unwrap();
    FileMeta::new(path, "/dl", st.size, st.modified, st.modified)
}

/// Archive dates are in the past, as in a real download.
const OLD: i64 = 1_700_000_000;

fn setup(name: &str, archive: &str) -> MemFs {
    let mut fs = MemFs::new();
    fs.add_file_with(format!("/dl/{name}"), archive.as_bytes(), OLD);
    fs
}

fn run(fs: &mut MemFs, j: &mut MemJournal, r: &Rule, path: &str) -> RunReport {
    let d = HashMap::new();
    let m = meta(fs, path);
    run_rule(fs, j, r, &m, &ctx(&d))
}

/// Paths in the order `MemFs::file_paths` lists them.
fn paths(v: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = v.iter().map(|s| p(s)).collect();
    v.sort();
    v
}

fn no_part_folders(fs: &MemFs) -> bool {
    fs.file_paths().iter().all(|f| !org_fs::in_part_folder(f)) && fs.list_dir(&p("/dl")).unwrap().iter().all(|e| !org_fs::in_part_folder(&e.path))
}

#[test]
fn extracts_into_a_folder_named_after_the_archive() {
    let mut fs = setup("photos.zip", "a.jpg=1\nsub/b.jpg=22\n");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(r.path, p("/dl/photos.zip"), "the archive stays where it is");
    assert_eq!(fs.file_paths(), paths(&["/dl/photos.zip", "/dl/photos/a.jpg", "/dl/photos/sub/b.jpg"]));
    assert_eq!(fs.stat("/dl/photos/sub/b.jpg").unwrap().size, 2);
    let e = &j.entries()[0];
    assert_eq!((e.kind, e.dst.clone(), e.status.clone()), (OpKind::Extract, Some(p("/dl/photos")), Status::Done));
    assert!(no_part_folders(&fs));
}

#[test]
fn a_single_top_folder_is_not_nested_again() {
    let mut fs = setup("photos.zip", "photos/a.jpg=1\nphotos/b.jpg=2\n");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(fs.file_paths(), paths(&["/dl/photos.zip", "/dl/photos/a.jpg", "/dl/photos/b.jpg"]));
    assert!(no_part_folders(&fs));
}

#[test]
fn relative_destination_is_taken_from_the_watched_folder() {
    let mut fs = setup("photos.zip", "a.jpg=1\n");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("Unpacked/{name}", Conflict::Rename)]), "/dl/photos.zip");
    assert!(r.ok(), "{:?}", r.steps);
    assert!(fs.stat("/dl/Unpacked/photos/a.jpg").is_some());
}

#[test]
fn an_existing_folder_is_never_mixed_into() {
    let mut fs = setup("photos.zip", "a.jpg=new\n");
    fs.add_file_with("/dl/photos/a.jpg", b"old", OLD);
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
    assert!(r.ok(), "{:?}", r.steps);
    assert_eq!(fs.stat("/dl/photos/a.jpg").unwrap().size, 3, "the existing folder is untouched");
    assert!(fs.stat("/dl/photos_2/a.jpg").is_some());

    let mut fs = setup("photos.zip", "a.jpg=new\n");
    fs.add_dir("/dl/photos");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Skip)]), "/dl/photos.zip");
    assert!(matches!(r.steps[0], StepStatus::Skipped(_)), "{:?}", r.steps);
    assert_eq!(fs.file_paths(), vec![p("/dl/photos.zip")]);
    assert!(j.entries().is_empty());
}

#[test]
fn a_failed_extraction_leaves_nothing_behind_and_stops_the_rule() {
    for archive in ["BROKEN", "ok.txt=1\n../escape.txt=evil\n"] {
        let mut fs = setup("bad.zip", archive);
        let mut j = MemJournal::new();
        let r = rule(vec![extract("{folder}/{name}", Conflict::Rename), Action::Trash { remove_empty: false }]);
        let rep = run(&mut fs, &mut j, &r, "/dl/bad.zip");
        assert!(!rep.ok());
        assert_eq!(rep.steps.len(), 1, "the archive is not trashed after a failure");
        assert_eq!(fs.file_paths(), vec![p("/dl/bad.zip")], "{archive:?}");
        assert!(!fs.has_dir("/dl/bad") && no_part_folders(&fs));
        assert!(matches!(j.entries()[0].status, Status::Failed(_)));
    }
}

#[test]
fn the_archive_can_go_to_the_bin_after_extraction() {
    let mut fs = setup("photos.zip", "a.jpg=1\n");
    let mut j = MemJournal::new();
    let r = rule(vec![extract("{folder}/{name}", Conflict::Rename), Action::Trash { remove_empty: false }]);
    let rep = run(&mut fs, &mut j, &r, "/dl/photos.zip");
    assert!(rep.ok() && rep.gone, "{:?}", rep.steps);
    assert_eq!(fs.file_paths(), vec![p("/dl/photos/a.jpg")]);
    assert_eq!(fs.trashed(), vec![p("/dl/photos.zip")]);
}

#[test]
fn later_volumes_of_a_split_archive_are_skipped() {
    for name in ["film.part2.rar", "film.part10.rar", "film.r00", "film.R01"] {
        let mut fs = setup(name, "x=1\n");
        let mut j = MemJournal::new();
        let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), &format!("/dl/{name}"));
        assert!(matches!(r.steps[0], StepStatus::Skipped(_)), "{name}: {:?}", r.steps);
    }
    for name in ["film.part1.rar", "film.rar", "parts.zip", "report.rtf"] {
        let mut fs = setup(name, "x=1\n");
        let mut j = MemJournal::new();
        let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), &format!("/dl/{name}"));
        assert_eq!(r.steps, vec![StepStatus::Done], "{name}");
    }
}

#[test]
fn undo_sends_the_folder_to_the_bin_and_remembers_the_archive() {
    let mut fs = setup("photos.zip", "a.jpg=1\nsub/b.jpg=2\n");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
    let id = j.by_run(r.run_id)[0].id;
    assert_eq!(undo(&mut fs, &mut j, id, false).unwrap(), UndoOutcome::Restored(p("/dl/photos.zip")));
    assert_eq!(fs.file_paths(), vec![p("/dl/photos.zip")]);
    assert_eq!(fs.trashed_dirs(), vec![p("/dl/photos")]);
    let st = fs.stat("/dl/photos.zip").unwrap();
    assert!(j.restored_to(1, &p("/dl/photos.zip"), st.size, st.modified), "the rule must not extract it again");
}

#[test]
fn undo_asks_first_when_the_folder_changed() {
    let mut fs = setup("photos.zip", "a.jpg=1\n");
    let mut j = MemJournal::new();
    let r = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
    let id = j.by_run(r.run_id)[0].id;
    fs.add_file("/dl/photos/notes.txt", 5, timestamp(NOW.0, NOW.1, NOW.2, 13, 0, 0));
    assert!(matches!(undo(&mut fs, &mut j, id, false).unwrap(), UndoOutcome::NeedsConfirmation(_)));
    assert!(fs.stat("/dl/photos/notes.txt").is_some());
    assert!(matches!(undo(&mut fs, &mut j, id, true).unwrap(), UndoOutcome::Restored(_)));
    assert_eq!(fs.file_paths(), vec![p("/dl/photos.zip")]);
}

/// Whatever step fails, recovery ends with either the complete folder or nothing, never a
/// half-extracted folder, and the archive is always kept.
#[test]
fn an_interruption_at_any_step_leaves_all_or_nothing() {
    for n in 0..12 {
        let mut fs = setup("photos.zip", "a.jpg=1\nsub/b.jpg=2\n");
        let mut j = MemJournal::new();
        fs.fail_at(n);
        let _ = run(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), "/dl/photos.zip");
        j.simulate_crash();
        fs.clear_failure();
        recover(&mut fs, &mut j);
        let files = fs.file_paths();
        let complete = paths(&["/dl/photos.zip", "/dl/photos/a.jpg", "/dl/photos/sub/b.jpg"]);
        assert!(files == complete || files == vec![p("/dl/photos.zip")], "n={n}: {files:?}");
        assert!(no_part_folders(&fs), "n={n}");
        assert!(j.in_progress().is_empty());
    }
}

#[test]
fn a_leftover_partial_folder_is_cleaned_up() {
    let mut fs = setup("photos.zip", "a.jpg=1\n");
    fs.add_file("/dl/photos.filepilot-part/half.jpg", 3, OLD);
    let mut j = MemJournal::new();
    j.begin(NewEntry {
        run_id: 1,
        rule_id: 1,
        ts: 0,
        kind: OpKind::Extract,
        src: p("/dl/photos.zip"),
        dst: Some(p("/dl/photos")),
        size: 8,
        mtime: OLD,
    });
    let st = recover(&mut fs, &mut j);
    assert!(matches!(st[0].1, Status::Failed(_)));
    assert_eq!(fs.file_paths(), vec![p("/dl/photos.zip")]);
    assert!(no_part_folders(&fs));
}

#[test]
fn the_preview_shows_the_folder_without_touching_the_disk() {
    let fs = setup("photos.zip", "a.jpg=1\n");
    let mut dry = DryFs::new(&fs);
    let d = HashMap::new();
    let r = rule(vec![extract("{folder}/{name}", Conflict::Rename), Action::Trash { remove_empty: false }]);
    let plan = plan_rule(&mut dry, &r, &meta(&fs, "/dl/photos.zip"), &ctx(&d));
    assert_eq!(plan.error, None);
    assert_eq!(plan.ops, vec![(OpKind::Extract, Some(p("/dl/photos"))), (OpKind::Trash, None)]);
    assert!(plan.gone);
    assert_eq!(fs.file_paths(), vec![p("/dl/photos.zip")]);
}

#[test]
fn files_being_extracted_are_ignored() {
    assert!(org_fs::in_part_folder(&p("/dl/photos.filepilot-part/a.jpg")));
    assert!(org_fs::in_part_folder(&p("/dl/Photos.FilePilot-Part/sub/b.jpg")));
    assert!(!org_fs::in_part_folder(&p("/dl/photos/a.jpg")));
}

#[test]
fn extract_follows_the_usual_action_rules() {
    let r = rule(vec![extract("{folder}\\{name}", Conflict::Skip)]);
    assert!(r.validate().is_ok());
    // Extract after Trash has no file to work on.
    let bad = rule(vec![Action::Trash { remove_empty: false }, extract("{folder}\\{name}", Conflict::Rename)]);
    assert!(bad.validate().is_err());
}

/// The same steps on the real disk, with the system `tar` (a tar archive, which both GNU
/// tar and the Windows tar read).
#[test]
fn extracts_on_the_real_disk() {
    fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in files {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..108].copy_from_slice(b"0000644\0");
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
    let base = std::env::temp_dir().join(format!("org-extract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let archive = base.join("photos.tar");
    std::fs::write(&archive, tar(&[("photos/a.txt", b"hello"), ("photos/sub/b.txt", b"world!")])).unwrap();
    let mut fs = org_fs::StdFs;
    let mut j = MemJournal::new();
    let d = HashMap::new();
    let st = fs.metadata(&archive).unwrap();
    let m = FileMeta::new(archive.to_str().unwrap(), base.to_str().unwrap(), st.size, st.modified, st.modified);
    let r = run_rule(&mut fs, &mut j, &rule(vec![extract("{folder}/{name}", Conflict::Rename)]), &m, &ctx(&d));
    if matches!(&r.steps[0], StepStatus::Failed(msg) if msg.contains("not found") || msg.contains("not available")) {
        return; // no tar on this system
    }
    assert!(r.ok(), "{:?}", r.steps);
    let out = base.join("photos");
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(out.join("sub").join("b.txt")).unwrap(), b"world!");
    assert!(!base.join("photos.filepilot-part").exists());
    assert!(archive.exists());
    let _ = std::fs::remove_dir_all(&base);
}
