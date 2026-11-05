//! End-to-end tests: create → open → list → extract → compare.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use nax_core::{create, Archive, CreateOptions, Error, ExtractOptions, Overwrite, Preset};

fn noop(_: &nax_core::Progress) {}

/// Deterministic pseudo-random bytes.
fn random_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn text(n_lines: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n_lines {
        v.extend_from_slice(format!("{i}: the quick brown fox jumps over the lazy dog {}\n", i % 13).as_bytes());
    }
    v
}

/// Builds a test tree and returns its root ("data").
fn make_tree(base: &Path) -> PathBuf {
    let root = base.join("data");
    let w = |rel: &str, bytes: &[u8]| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    };
    w("readme.txt", b"hello nanarchive");
    w("empty.bin", b"");
    w("docs/Привет мир.txt", "Кириллица работает ✓".as_bytes());
    w("docs/big text.log", &text(60_000)); // ~3 MB, spans several blocks
    w("bin/random.dat", &random_bytes(1_500_000, 7)); // incompressible
    w("bin/copy.dat", &random_bytes(1_500_000, 7)); // duplicate content
    w("deep/a/b/c/d/e/f.txt", b"deep");
    fs::create_dir_all(root.join("empty folder")).unwrap();
    root
}

/// Maps relative path → contents (None for folders).
fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in walkdir(root) {
        let rel = e.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if e.is_dir() {
            out.insert(rel, None);
        } else {
            out.insert(rel, Some(fs::read(&e).unwrap()));
        }
    }
    out
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            v.push(p);
        }
    }
    v
}

fn opts(preset: Preset) -> CreateOptions {
    CreateOptions {
        preset,
        threads: Some(3),
        block_size: Some(1 << 20), // small blocks: forces multi-block files
        ..Default::default()
    }
}

#[test]
fn round_trip_every_preset() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let original = snapshot(&root);

    for preset in Preset::ALL {
        let archive = tmp.path().join(format!("test-{preset}.nax"));
        let stats = create(&archive, std::slice::from_ref(&root), &opts(preset), &mut noop).unwrap();
        assert_eq!(stats.files, 7, "{preset}");
        assert!(stats.warnings.is_empty(), "{:?}", stats.warnings);
        assert!(stats.blocks > 3, "{preset}: expected several blocks");

        let a = Archive::open(&archive).unwrap();
        assert_eq!(a.entries().len(), stats.files as usize + stats.dirs as usize);
        assert!(a.entries().iter().any(|e| e.path == "data/docs/Привет мир.txt"));

        let t = a.test(Some(2), None, &mut noop).unwrap();
        assert_eq!(t.files, 7);

        let out = tmp.path().join(format!("out-{preset}"));
        let x = a.extract(&out, &ExtractOptions::default(), &mut noop).unwrap();
        assert_eq!(x.files, 7);
        assert_eq!(snapshot(&out.join("data")), original, "{preset}: contents differ");

        if preset != Preset::Store {
            assert!(stats.archive_size < stats.bytes_in * 7 / 10, "{preset} compressed too little");
        }
    }
}

#[test]
fn long_range_matches_shrink_far_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dup");
    fs::create_dir_all(&root).unwrap();
    let noise = random_bytes(3_000_000, 42);
    fs::write(root.join("a.bin"), &noise).unwrap();
    fs::write(root.join("b.txt"), text(40_000)).unwrap(); // something in between
    fs::write(root.join("c.bin"), &noise).unwrap();

    for preset in [Preset::Balanced, Preset::Max] {
        let archive = tmp.path().join(format!("dup-{preset}.nax"));
        let s = create(&archive, &[root.clone()], &opts(preset), &mut noop).unwrap();
        let a = Archive::open(&archive).unwrap();
        assert!(a.blocks().iter().any(|b| b.uses_long_range()), "{preset}");
        // Random data can't compress, so the second copy must be (nearly) free.
        assert!(s.archive_size < 3_300_000, "{preset}: {}", s.archive_size);
        let out = tmp.path().join(format!("out-{preset}"));
        a.extract(&out, &ExtractOptions::default(), &mut noop).unwrap();
        assert_eq!(fs::read(out.join("dup/c.bin")).unwrap(), noise);

        // Extracting only the last file still works (needs earlier blocks).
        let out2 = tmp.path().join(format!("out2-{preset}"));
        let only = ExtractOptions { only: vec!["dup/c.bin".into()], ..Default::default() };
        a.extract(&out2, &only, &mut noop).unwrap();
        assert_eq!(fs::read(out2.join("dup/c.bin")).unwrap(), noise);
    }
}

#[test]
fn modification_times_are_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let file = root.join("readme.txt");
    let when = filetime::FileTime::from_unix_time(1_600_000_000, 123_456_700);
    filetime::set_file_mtime(&file, when).unwrap();

    let archive = tmp.path().join("t.nax");
    create(&archive, &[root], &opts(Preset::Fast), &mut noop).unwrap();
    let out = tmp.path().join("out");
    Archive::open(&archive)
        .unwrap()
        .extract(&out, &ExtractOptions::default(), &mut noop)
        .unwrap();
    let meta = fs::metadata(out.join("data/readme.txt")).unwrap();
    assert_eq!(filetime::FileTime::from_last_modification_time(&meta), when);
}

#[test]
fn single_files_and_selective_extract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = tmp.path().join("t.nax");
    let inputs = vec![root.join("readme.txt"), root.join("docs")];
    create(&archive, &inputs, &opts(Preset::Balanced), &mut noop).unwrap();

    let a = Archive::open(&archive).unwrap();
    let names: Vec<&str> = a.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"readme.txt"));
    assert!(names.contains(&"docs/big text.log"));

    let out = tmp.path().join("out");
    let x = a
        .extract(
            &out,
            &ExtractOptions { only: vec!["docs\\big text.log".into()], ..Default::default() },
            &mut noop,
        )
        .unwrap();
    assert_eq!(x.files, 1);
    assert_eq!(fs::read(out.join("docs/big text.log")).unwrap(), text(60_000));
    assert!(!out.join("readme.txt").exists());

    let missing = a.extract(&out, &ExtractOptions { only: vec!["nope".into()], ..Default::default() }, &mut noop);
    assert!(matches!(missing, Err(Error::InvalidInput(_))));
}

#[test]
fn overwrite_modes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = tmp.path().join("t.nax");
    create(&archive, &[root.join("readme.txt")], &opts(Preset::Fast), &mut noop).unwrap();
    let a = Archive::open(&archive).unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("readme.txt"), b"mine").unwrap();

    let x = a.extract(&out, &ExtractOptions::default(), &mut noop).unwrap();
    assert_eq!((x.files, x.skipped), (0, 1));
    assert_eq!(fs::read(out.join("readme.txt")).unwrap(), b"mine");

    let fail = ExtractOptions { overwrite: Overwrite::Fail, ..Default::default() };
    assert!(matches!(a.extract(&out, &fail, &mut noop), Err(Error::AlreadyExists(_))));

    let replace = ExtractOptions { overwrite: Overwrite::Replace, ..Default::default() };
    a.extract(&out, &replace, &mut noop).unwrap();
    assert_eq!(fs::read(out.join("readme.txt")).unwrap(), b"hello nanarchive");
}

#[test]
fn refuses_to_overwrite_archive_unless_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = tmp.path().join("t.nax");
    create(&archive, std::slice::from_ref(&root), &opts(Preset::Fast), &mut noop).unwrap();
    let again = create(&archive, std::slice::from_ref(&root), &opts(Preset::Fast), &mut noop);
    assert!(matches!(again, Err(Error::AlreadyExists(_))));
    let o = CreateOptions { overwrite: true, ..opts(Preset::Fast) };
    create(&archive, &[root], &o, &mut noop).unwrap();
}

#[test]
fn archive_inside_input_folder_skips_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = root.join("self.nax");
    create(&archive, std::slice::from_ref(&root), &opts(Preset::Fast), &mut noop).unwrap();
    let a = Archive::open(&archive).unwrap();
    assert!(a.entries().iter().all(|e| !e.path.ends_with("self.nax") && !e.path.ends_with(".tmp")));
}

#[test]
fn detects_corruption() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = tmp.path().join("t.nax");
    create(&archive, &[root], &opts(Preset::Max), &mut noop).unwrap();
    let good = fs::read(&archive).unwrap();

    // Flip one byte inside the data area.
    let mut bad = good.clone();
    bad[100] ^= 0x01;
    let p = tmp.path().join("bad-data.nax");
    fs::write(&p, &bad).unwrap();
    let a = Archive::open(&p).unwrap(); // file list is still fine
    assert!(matches!(a.test(None, None, &mut noop), Err(Error::Corrupt(_))));
    let out = tmp.path().join("out-bad");
    assert!(matches!(a.extract(&out, &ExtractOptions::default(), &mut noop), Err(Error::Corrupt(_))));

    // Damage the file list.
    let mut bad = good.clone();
    let n = bad.len();
    bad[n - 70] ^= 0x01;
    let p = tmp.path().join("bad-index.nax");
    fs::write(&p, &bad).unwrap();
    assert!(matches!(Archive::open(&p), Err(Error::Corrupt(_))));

    // Truncated download.
    let p = tmp.path().join("short.nax");
    fs::write(&p, &good[..good.len() / 2]).unwrap();
    assert!(Archive::open(&p).is_err());

    // Not an archive at all.
    let p = tmp.path().join("fake.nax");
    fs::write(&p, b"PK\x03\x04 definitely a zip").unwrap();
    assert!(matches!(Archive::open(&p), Err(Error::BadMagic)));
}

#[test]
fn cancel_leaves_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_tree(tmp.path());
    let archive = tmp.path().join("t.nax");
    let cancel = Arc::new(AtomicBool::new(true));
    let o = CreateOptions { cancel: Some(cancel), ..opts(Preset::Max) };
    assert!(matches!(create(&archive, &[root], &o, &mut noop), Err(Error::Cancelled)));
    assert!(!archive.exists());
    assert!(!tmp.path().join("t.nax.tmp").exists());
}

#[test]
fn empty_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("nothing");
    fs::create_dir_all(&dir).unwrap();
    let archive = tmp.path().join("t.nax");
    let s = create(&archive, &[dir], &opts(Preset::Balanced), &mut noop).unwrap();
    assert_eq!((s.files, s.dirs, s.blocks), (0, 1, 0));
    let a = Archive::open(&archive).unwrap();
    a.test(None, None, &mut noop).unwrap();
    let out = tmp.path().join("out");
    a.extract(&out, &ExtractOptions::default(), &mut noop).unwrap();
    assert!(out.join("nothing").is_dir());
}
