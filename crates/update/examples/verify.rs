//! Used by the release build: checks a signed installer the way the app will, with the
//! public key built into it. `cargo run -p org-update --example verify -- SETUP.exe VERSION`
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [setup, version] = args.as_slice() else {
        eprintln!("usage: verify SETUP.exe VERSION");
        exit(2);
    };
    let Some(version) = org_update::Version::parse(version) else {
        eprintln!("not a version: {version}");
        exit(2);
    };
    let data = std::fs::read(setup).unwrap_or_else(|e| {
        eprintln!("{setup}: {e}");
        exit(1)
    });
    let sig = std::fs::read_to_string(format!("{setup}.minisig")).unwrap_or_else(|e| {
        eprintln!("{setup}.minisig: {e}");
        exit(1)
    });
    let name = std::path::Path::new(setup).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name != org_update::setup_name(version) {
        eprintln!("{name} is not the installer name the app expects ({})", org_update::setup_name(version));
        exit(1);
    }
    match org_update::verify_signature(&data, &sig, &name, version, org_update::PUBLIC_KEY) {
        Ok(()) => println!("{name}: signature ok, sha256 {}", org_core::secret::to_hex(&org_core::secret::sha256(&[&data]))),
        Err(e) => {
            eprintln!("{name}: {e}");
            exit(1);
        }
    }
}
