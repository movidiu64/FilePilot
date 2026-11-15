fn main() {
    for p in std::env::args().skip(1) {
        let d = std::fs::read(&p).unwrap();
        let ext = std::path::Path::new(&p).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let t0 = std::time::Instant::now();
        let t = org_content::extract(&ext, &d);
        eprintln!("== {p} ({} KB, {:?})", d.len() / 1024, t0.elapsed());
        println!("{}", t.unwrap_or_else(|| "<NONE>".into()));
    }
}
