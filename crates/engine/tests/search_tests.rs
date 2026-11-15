//! Searching the watched folders.

use std::path::PathBuf;

use org_core::WatchedFolder;
use org_engine::search::*;
use org_fs::MemFs;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn setup() -> MemFs {
    let mut fs = MemFs::new();
    fs.add_file("/dl/Invoice-0042.pdf", 120_000, 1_000);
    fs.add_file("/dl/invoice-0043.PDF", 80_000, 2_000);
    fs.add_file("/dl/holiday photo.jpg", 3_000_000, 3_000);
    fs.add_file("/dl/report final.docx", 40_000, 4_000);
    fs.add_file("/dl/sub/Invoice-0044.pdf", 10_000, 5_000);
    fs.add_file("/dl/movie.mkv.part", 9, 6_000);
    fs.add_file("/docs/contract.pdf", 50_000, 7_000);
    fs
}

fn index(fs: &MemFs, folders: &[WatchedFolder]) -> FileIndex {
    FileIndex::build(fs, folders, &mut |_| true).unwrap()
}

fn names<'a>(r: &(Vec<&'a IndexEntry>, usize)) -> Vec<&'a str> {
    r.0.iter().map(|e| e.name()).collect()
}

#[test]
fn words_extensions_sizes_and_dates() {
    let fs = setup();
    let idx = index(&fs, &[WatchedFolder { path: p("/dl"), recursive: true }]);
    assert_eq!(idx.entries.len(), 5, "partial downloads are not listed");
    let q = |text: &str| Query { text: text.into(), ..Query::default() };
    let s = |q: &Query| idx.search(q, SortBy::Name, false, 100);
    assert_eq!(names(&s(&q("invoice"))), vec!["Invoice-0042.pdf", "invoice-0043.PDF", "Invoice-0044.pdf"]);
    assert_eq!(names(&s(&q("final REPORT"))), vec!["report final.docx"], "all words, any order, any case");
    assert_eq!(names(&s(&q("invoice-00?3*"))), vec!["invoice-0043.PDF"], "wildcards match the whole name");
    assert_eq!(names(&s(&q("*.jpg"))), vec!["holiday photo.jpg"]);
    assert_eq!(s(&q("nothing")).1, 0);
    let by_ext = Query { extensions: ".PDF, docx".into(), ..Query::default() };
    assert_eq!(s(&by_ext).1, 4);
    let big = Query { min_size: Some(100_000), ..Query::default() };
    assert_eq!(names(&s(&big)), vec!["holiday photo.jpg", "Invoice-0042.pdf"]);
    let small = Query { max_size: Some(40_000), ..Query::default() };
    assert_eq!(names(&s(&small)), vec!["Invoice-0044.pdf", "report final.docx"]);
    let recent = Query { modified_after: Some(3_000), modified_before: Some(5_000), ..Query::default() };
    assert_eq!(names(&s(&recent)), vec!["holiday photo.jpg", "report final.docx"]);
}

#[test]
fn sorting_and_limits() {
    let fs = setup();
    let idx = index(&fs, &[WatchedFolder { path: p("/dl"), recursive: true }]);
    let all = Query::default();
    let (top, total) = idx.search(&all, SortBy::Size, true, 2);
    assert_eq!(total, 5);
    assert_eq!(top.iter().map(|e| e.size).collect::<Vec<_>>(), vec![3_000_000, 120_000], "the two largest");
    let (newest, _) = idx.search(&all, SortBy::Modified, true, 1);
    assert_eq!(newest[0].name(), "Invoice-0044.pdf");
    let (by_folder, _) = idx.search(&all, SortBy::Folder, false, 10);
    assert_eq!(by_folder.last().unwrap().path, p("/dl/sub/Invoice-0044.pdf"));
}

#[test]
fn folders_without_subfolders_overlaps_and_missing_drives() {
    let fs = setup();
    let idx = index(
        &fs,
        &[
            WatchedFolder { path: p("/dl"), recursive: false },
            WatchedFolder { path: p("/dl"), recursive: true },
            WatchedFolder { path: p("/dl/sub"), recursive: true },
            WatchedFolder { path: p("/docs"), recursive: false },
            WatchedFolder { path: p("/x:/gone"), recursive: true },
        ],
    );
    assert_eq!(idx.entries.len(), 6, "each file once");
    assert_eq!(idx.unavailable, vec![p("/x:/gone")]);
    let only_top = index(&fs, &[WatchedFolder { path: p("/dl"), recursive: false }]);
    assert_eq!(only_top.entries.len(), 4);
}

#[test]
fn building_can_be_stopped() {
    let mut fs = MemFs::new();
    for d in 0..200 {
        fs.add_file(format!("/big/d{d}/f.txt"), 1, 1);
    }
    let mut calls = 0;
    let r = FileIndex::build(&fs, &[WatchedFolder { path: p("/big"), recursive: true }], &mut |_| {
        calls += 1;
        false
    });
    assert!(r.is_none());
    assert_eq!(calls, 1);
}

#[test]
fn the_index_follows_moves_without_listing_again() {
    let mut fs = setup();
    let folders = [WatchedFolder { path: p("/dl"), recursive: true }, WatchedFolder { path: p("/docs"), recursive: false }];
    let mut idx = index(&fs, &folders);
    let all = |idx: &FileIndex, text: &str| {
        let q = Query { text: text.into(), ..Query::default() };
        idx.search(&q, SortBy::Name, false, 100).0.iter().map(|e| e.path.clone()).collect::<Vec<_>>()
    };
    // A rule moves an invoice into a subfolder, another one leaves the folders, a copy appears.
    fs.add_dir("/dl/Invoices");
    org_fs::Fs::rename(&mut fs, &p("/dl/Invoice-0042.pdf"), &p("/dl/Invoices/Invoice-0042.pdf")).unwrap();
    org_fs::Fs::remove_file(&mut fs, &p("/dl/invoice-0043.PDF")).unwrap();
    fs.add_file("/docs/contract copy.pdf", 50_000, 8_000);
    fs.add_file("/docs/deep/not-listed.pdf", 1, 1);
    idx.update(
        &fs,
        &[
            p("/dl/Invoice-0042.pdf"),
            p("/dl/Invoices/Invoice-0042.pdf"),
            p("/dl/invoice-0043.PDF"),
            p("/elsewhere/invoice-0043.PDF"),
            p("/docs/contract copy.pdf"),
            p("/docs/deep/not-listed.pdf"),
            p("/dl/Invoices"),
        ],
    );
    assert_eq!(all(&idx, "invoice"), vec![p("/dl/Invoices/Invoice-0042.pdf"), p("/dl/sub/Invoice-0044.pdf")]);
    assert_eq!(all(&idx, "contract"), vec![p("/docs/contract copy.pdf"), p("/docs/contract.pdf")], "not below a folder without subfolders");
    assert!(idx.entries.windows(2).all(|w| w[0].path < w[1].path), "kept in order");
    idx.forget(&p("/docs/contract.pdf"));
    assert_eq!(all(&idx, "contract").len(), 1);
}
