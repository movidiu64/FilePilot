use org_update::*;

/// The key the files in tests/data are signed with (tools/sign-release.py keygen); the app
/// trusts only PUBLIC_KEY, never this one.
const TEST_KEY: &str = "RWQOsQAEP/wvl52Y5mdtH3kVMol3w7/jCPmEk+L/ycBVXBWzrM3/Iflj";
const DATA: &[u8] = include_bytes!("data/FilePilot_9.0.0_x64-setup.exe");
const SIG: &str = include_str!("data/FilePilot_9.0.0_x64-setup.exe.minisig");
const SHA256: &str = "d2dc62f145a45cc2e32c85ca448f5eed4ea9791f4e0dcaba9d9fd6e4dc5fecc3";
const NAME: &str = "FilePilot_9.0.0_x64-setup.exe";

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// GitHub's answer, trimmed to the fields that matter (as of the 1.3.0 release).
fn latest(tag: &str, assets: &str) -> String {
    format!(
        r####"{{"url":"https://api.github.com/repos/movidiu64/FilePilot/releases/1","html_url":"https://github.com/movidiu64/FilePilot/releases/tag/{tag}",
        "tag_name":"{tag}","name":"FilePilot","draft":false,"prerelease":false,"created_at":"2026-11-22T10:00:00Z",
        "body":"### Added\r\n- **Recent files** in the tray.\r\n","assets":[{assets}]}}"####
    )
}

fn asset(name: &str, url: &str, digest: Option<&str>, size: u64) -> String {
    let d = digest.map_or("null".to_string(), |d| format!("\"{d}\""));
    format!(r#"{{"name":"{name}","size":{size},"digest":{d},"content_type":"application/octet-stream","browser_download_url":"{url}"}}"#)
}

fn good_assets() -> String {
    let base = "https://github.com/movidiu64/FilePilot/releases/download/v9.0.0";
    [
        asset("FilePilot-9.0.0-portable.zip", &format!("{base}/FilePilot-9.0.0-portable.zip"), Some("sha256:00"), 10),
        asset(NAME, &format!("{base}/{NAME}"), Some(&format!("sha256:{}", SHA256.to_uppercase())), DATA.len() as u64),
        asset(&format!("{NAME}.minisig"), &format!("{base}/{NAME}.minisig"), Some("sha256:11"), SIG.len() as u64),
    ]
    .join(",")
}

#[test]
fn versions() {
    assert_eq!(v("v1.10.0"), Version { major: 1, minor: 10, patch: 0 });
    assert!(v("1.10.0") > v("1.9.9") && v("2.0.0") > v("1.99.99") && v("1.3.1") > v("1.3.0"));
    for bad in ["1.2", "1.2.3.4", "1.2.3-beta", "01.2.3", "1..3", "", "v", "1.2.x", "+1.2.3"] {
        assert_eq!(Version::parse(bad), None, "{bad}");
    }
    assert_eq!(v("1.2.3").to_string(), "1.2.3");
    assert_eq!(setup_name(v("1.4.0")), "FilePilot_1.4.0_x64-setup.exe");
    assert_eq!(page_url(v("1.4.0")), "https://github.com/movidiu64/FilePilot/releases/tag/v1.4.0");
    let _ = Version::current();
}

#[test]
fn reads_the_latest_release() {
    let r = parse_latest(&latest("v9.0.0", &good_assets())).unwrap();
    assert_eq!(r.version, v("9.0.0"));
    assert_eq!(r.notes, "### Added\n- **Recent files** in the tray.\n");
    assert_eq!(r.setup.name, NAME);
    assert_eq!(r.setup.url, format!("https://github.com/movidiu64/FilePilot/releases/download/v9.0.0/{NAME}"));
    assert_eq!(r.setup.sha256.as_deref(), Some(SHA256), "lowercased");
    assert_eq!(r.setup.size, DATA.len() as u64);
    assert_eq!(r.signature.name, format!("{NAME}.minisig"));
}

#[test]
fn refuses_releases_that_are_not_ours() {
    let base = "https://github.com/movidiu64/FilePilot/releases/download/v9.0.0";
    let sig = asset(&format!("{NAME}.minisig"), &format!("{base}/{NAME}.minisig"), None, 300);
    let setup = |url: &str, digest: Option<&str>| asset(NAME, url, digest, 10);
    let digest = format!("sha256:{SHA256}");
    let cases = [
        ("no installer", latest("v9.0.0", &sig)),
        ("no signature", latest("v9.0.0", &setup(&format!("{base}/{NAME}"), Some(&digest)))),
        ("no SHA-256", latest("v9.0.0", &[setup(&format!("{base}/{NAME}"), None), sig.clone()].join(","))),
        ("bad SHA-256", latest("v9.0.0", &[setup(&format!("{base}/{NAME}"), Some("sha256:abc")), sig.clone()].join(","))),
        ("another host", latest("v9.0.0", &[setup(&format!("https://evil.example/{NAME}"), Some(&digest)), sig.clone()].join(","))),
        ("another repo", latest("v9.0.0", &[setup(&format!("https://github.com/someone/FilePilot/releases/download/v9.0.0/{NAME}"), Some(&digest)), sig.clone()].join(","))),
        ("another version's folder", latest("v9.0.0", &[setup(&format!("https://github.com/movidiu64/FilePilot/releases/download/v8.0.0/{NAME}"), Some(&digest)), sig.clone()].join(","))),
        ("plain http", latest("v9.0.0", &[setup(&format!("http://github.com/movidiu64/FilePilot/releases/download/v9.0.0/{NAME}"), Some(&digest)), sig.clone()].join(","))),
        ("a tag that is no version", latest("nightly", &good_assets())),
        ("a pre-release tag", latest("v9.0.0-rc1", &good_assets())),
        ("draft", latest("v9.0.0", &good_assets()).replace(r#""draft":false"#, r#""draft":true"#)),
        ("pre-release", latest("v9.0.0", &good_assets()).replace(r#""prerelease":false"#, r#""prerelease":true"#)),
        ("not JSON", "<html>rate limited</html>".to_string()),
    ];
    for (what, text) in cases {
        assert!(matches!(parse_latest(&text), Err(Error::BadRelease(_))), "{what}: {:?}", parse_latest(&text));
    }
}

#[test]
fn the_signature_is_checked_with_the_key_file_and_version() {
    assert_eq!(verify_signature(DATA, SIG, NAME, v("9.0.0"), TEST_KEY), Ok(()));
    let mut changed = DATA.to_vec();
    changed[100] ^= 1;
    let err = |r: Result<(), Error>| matches!(r, Err(Error::Signature(_)));
    assert!(err(verify_signature(&changed, SIG, NAME, v("9.0.0"), TEST_KEY)), "a changed file");
    let other = include_str!("data/other-key.minisig");
    assert!(err(verify_signature(DATA, other, NAME, v("9.0.0"), TEST_KEY)), "another key");
    assert!(err(verify_signature(DATA, SIG, NAME, v("9.0.0"), PUBLIC_KEY)), "the release key did not sign the test file");
    assert!(err(verify_signature(DATA, SIG, NAME, v("9.0.1"), TEST_KEY)), "signed for another version");
    assert!(err(verify_signature(DATA, SIG, "FilePilot_9.0.1_x64-setup.exe", v("9.0.0"), TEST_KEY)), "signed for another file");
    // The same bytes signed as an older release cannot pass for the newer one.
    let older = include_str!("data/older-version.minisig");
    assert!(err(verify_signature(DATA, older, NAME, v("9.0.0"), TEST_KEY)));
    assert!(err(verify_signature(DATA, older, NAME, v("8.0.0"), TEST_KEY)), "and its file name is the 8.0.0 one");
    // A signed comment cannot be edited.
    let edited = SIG.replace("version:9.0.0", "version:9.0.1");
    assert!(err(verify_signature(DATA, &edited, NAME, v("9.0.1"), TEST_KEY)));
    // Windows line ends (a checkout with autocrlf) still read.
    let lf = SIG.replace('\r', "");
    assert_eq!(verify_signature(DATA, &lf, NAME, v("9.0.0"), TEST_KEY), Ok(()));
    assert_eq!(verify_signature(DATA, &lf.replace('\n', "\r\n"), NAME, v("9.0.0"), TEST_KEY), Ok(()));
    assert_eq!(verify_signature(DATA, &lf.replace('\n', "\r\r\n"), NAME, v("9.0.0"), TEST_KEY), Ok(()));
    assert!(err(verify_signature(DATA, "garbage", NAME, v("9.0.0"), TEST_KEY)));
    assert!(err(verify_signature(DATA, "", NAME, v("9.0.0"), TEST_KEY)));
}

#[test]
fn the_installer_must_match_github_and_the_release_key() {
    let r = parse_latest(&latest("v9.0.0", &good_assets())).unwrap();
    // Size and hash match, but the test file is not signed with the release key.
    assert!(matches!(verify_installer(DATA, SIG, &r), Err(Error::Signature(_))));
    let mut bad = r.clone();
    bad.setup.sha256 = Some("0".repeat(64));
    assert_eq!(verify_installer(DATA, SIG, &bad), Err(Error::Hash));
    let mut bad = r.clone();
    bad.setup.size += 1;
    assert_eq!(verify_installer(DATA, SIG, &bad), Err(Error::Hash));
    let mut bad = r;
    bad.setup.sha256 = None;
    assert_eq!(verify_installer(DATA, SIG, &bad), Err(Error::Hash));
}

#[test]
fn the_state_file() {
    let st = State { checked_at: 1_000, latest: Some(v("99.0.0")), notes: "a\n\"b\"".into(), error: None };
    assert_eq!(State::from_json(&st.to_json()), st);
    assert_eq!(st.available(), Some(v("99.0.0")));
    assert_eq!(State { latest: Some(Version::current()), ..st.clone() }.available(), None, "the same version is not an update");
    assert!(!st.due(1_000 + CHECK_EVERY - 1) && st.due(1_000 + CHECK_EVERY), "once a day");
    assert!(st.due(999), "a clock set back");
    let failed = State { error: Some("network".into()), ..st.clone() };
    assert!(!failed.due(1_000 + RETRY_AFTER - 1) && failed.due(1_000 + RETRY_AFTER), "sooner after a failure");
    assert_eq!(State::from_json("not json"), State::default());
    assert!(State::default().due(1_800_000_000), "never checked");

    let dir = std::env::temp_dir().join(format!("fp-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(STATE_FILE);
    write_state(&path, &st).unwrap();
    assert_eq!(read_state(&path), st);
    assert_eq!(read_state(&dir.join("missing.json")), State::default());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn only_https_is_fetched() {
    assert!(matches!(fetch("http://example.com/", None, 1), Err(Error::BadRelease(_))));
    assert!(matches!(fetch("file:///etc/passwd", None, 1), Err(Error::BadRelease(_))));
}

#[test]
fn errors_have_codes() {
    assert_eq!(Error::NoCurl.code(), "no_curl");
    assert_eq!(Error::Network("timeout".into()).to_string(), "network: timeout");
    assert_eq!(Error::Hash.to_string(), "hash");
}
