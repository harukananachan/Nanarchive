//! End-to-end tests for Phase 5: passwords, split archives, recovery records,
//! and that damaged or hostile archives fail safely.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use nax_core::crypto::KdfParams;
use nax_core::{create, recovery, Archive, CreateOptions, Error, ExtractOptions, Preset};

fn noop(_: &nax_core::Progress) {}

/// Cheap key derivation so the tests run quickly (real archives use 128 MiB).
const FAST_KDF: KdfParams = KdfParams { m_kib: 256, t: 1, p: 1 };

fn bytes(n: usize, seed: u64) -> Vec<u8> {
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

fn tree(base: &Path) -> PathBuf {
    let root = base.join("secret stuff");
    let w = |rel: &str, data: &[u8]| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    };
    let text: Vec<u8> = (0..40_000).flat_map(|i| format!("line {i} of my diary, very private\n").into_bytes()).collect();
    w("diary-confidential.txt", &text);
    w("photos/holiday-beach.raw", &bytes(700_000, 3));
    w("tax/2026-return.pdf", &bytes(300_000, 9));
    root
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn opts() -> CreateOptions {
    CreateOptions { preset: Preset::Fast, threads: Some(3), block_size: Some(256 << 10), ..Default::default() }
}

fn extract_all(a: &Archive, dest: &Path) -> BTreeMap<String, Vec<u8>> {
    a.extract(dest, &ExtractOptions::default(), &mut noop).unwrap();
    snapshot(&dest.join("secret stuff"))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn encrypted_round_trip_hides_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("locked.nax");
    let o = CreateOptions { password: Some("Пароль с пробелами ✓".into()), kdf: Some(FAST_KDF), ..opts() };
    create(&arc, &[root.clone()], &o, &mut noop).unwrap();

    // Neither file names nor contents are visible in the file.
    let raw = fs::read(&arc).unwrap();
    for secret in [&b"diary-confidential"[..], b"holiday-beach", b"2026-return", b"very private", b"secret stuff"] {
        assert!(!contains(&raw, secret), "{:?} leaked", String::from_utf8_lossy(secret));
    }

    assert!(matches!(Archive::open(&arc), Err(Error::PasswordRequired)));
    assert!(matches!(Archive::open_with(&arc, Some("wrong")), Err(Error::WrongPassword)));
    let a = Archive::open_with(&arc, Some("Пароль с пробелами ✓")).unwrap();
    assert!(a.is_encrypted());
    assert_eq!(extract_all(&a, &dir.path().join("out")), snapshot(&root));
    a.test(None, None, &mut noop).unwrap();
}

#[test]
fn tampering_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("locked.nax");
    let o = CreateOptions { password: Some("pw".into()), kdf: Some(FAST_KDF), ..opts() };
    create(&arc, &[root], &o, &mut noop).unwrap();
    let good = fs::read(&arc).unwrap();

    // Flip one bit in many places: open + test must fail cleanly, never panic
    // and never report success.
    let step = good.len() / 97;
    for pos in (0..good.len()).step_by(step) {
        let mut bad = good.clone();
        bad[pos] ^= 0x10;
        fs::write(&arc, &bad).unwrap();
        let r = Archive::open_with(&arc, Some("pw")).and_then(|a| a.test(None, None, &mut noop).map(|_| ()));
        assert!(r.is_err(), "flip at {pos} of {} went unnoticed", good.len());
    }

    // Turning encryption off in the header doesn't give access either.
    let mut bad = good.clone();
    bad[6] = 0;
    fs::write(&arc, &bad).unwrap();
    assert!(Archive::open(&arc).is_err());
}

#[test]
fn split_archive_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("big.nax");
    let o = CreateOptions { volume_size: Some(200_000), ..opts() };
    let s = create(&arc, &[root.clone()], &o, &mut noop).unwrap();
    assert!(s.outputs.len() >= 3, "{:?}", s.outputs);
    assert!(s.outputs[0].to_string_lossy().ends_with("big.part1.nax"));
    assert!(!arc.exists());
    assert_eq!(s.archive_size, s.outputs.iter().map(|p| fs::metadata(p).unwrap().len()).sum::<u64>());

    // Any part opens the whole set.
    let a = Archive::open(&s.outputs[1]).unwrap();
    assert_eq!(a.parts(), s.outputs.len());
    assert_eq!(extract_all(&a, &dir.path().join("out")), snapshot(&root));

    // A missing part is reported clearly.
    fs::rename(&s.outputs[1], dir.path().join("moved")).unwrap();
    let e = Archive::open(&s.outputs[0]).err().unwrap().to_string();
    assert!(e.contains("part 2"), "{e}");
}

#[test]
fn recovery_repairs_damage_then_extracts() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("safe.nax");
    let o = CreateOptions { recovery_percent: 5.0, password: Some("pw".into()), kdf: Some(FAST_KDF), ..opts() };
    let s = create(&arc, &[root.clone()], &o, &mut noop).unwrap();
    let info = s.recovery.unwrap();
    assert!(info.percent() >= 5.0);
    let a = Archive::open_with(&arc, Some("pw")).unwrap();
    assert_eq!(a.recovery(), Some(info));
    drop(a);
    assert!(recovery::check(&arc, None, &mut noop).unwrap().intact());

    // Scratch the middle of the file (inside the data blocks).
    let mut b = fs::read(&arc).unwrap();
    let mid = b.len() / 3;
    for x in &mut b[mid..mid + 2000] {
        *x = 0;
    }
    fs::write(&arc, &b).unwrap();
    assert!(Archive::open_with(&arc, Some("pw")).unwrap().test(None, None, &mut noop).is_err());

    let r = recovery::repair(&arc, None, &mut noop).unwrap();
    assert!(r.repairable && r.damaged_shards > 0, "{r:?}");
    let fixed = &r.output[0];
    assert!(fixed.to_string_lossy().ends_with("safe.repaired.nax"));
    let a = Archive::open_with(fixed, Some("pw")).unwrap();
    assert_eq!(extract_all(&a, &dir.path().join("out")), snapshot(&root));
}

#[test]
fn recovery_on_split_archive_rebuilds_lost_part() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("set.nax");
    // ~25% parity: enough to rebuild one whole part of a 5+ part set.
    let o = CreateOptions { recovery_percent: 25.0, volume_size: Some(150_000), ..opts() };
    let s = create(&arc, &[root.clone()], &o, &mut noop).unwrap();
    assert!(s.outputs.len() >= 5);
    fs::remove_file(&s.outputs[2]).unwrap();
    assert!(Archive::open(&s.outputs[0]).is_err());

    let r = recovery::repair(&s.outputs[0], None, &mut noop).unwrap();
    assert!(r.repairable, "{r:?}");
    assert!(r.output[0].to_string_lossy().ends_with("set.repaired.part1.nax"));
    let a = Archive::open(&r.output[0]).unwrap();
    assert_eq!(extract_all(&a, &dir.path().join("out")), snapshot(&root));
}

#[test]
fn cut_off_archive_with_recovery_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("cut.nax");
    let o = CreateOptions { recovery_percent: 10.0, ..opts() };
    let s = create(&arc, &[root.clone()], &o, &mut noop).unwrap();
    let info = s.recovery.unwrap();
    // Lose the end of the archive part and everything after (like a broken
    // download), but keep the first header copy readable: cut inside the parity.
    let b = fs::read(&arc).unwrap();
    let keep = (info.protected_len + info.record_len() / 2) as usize;
    let mut cut = b[..keep].to_vec();
    // Damage the archive's own end too (footer + file list).
    let p = info.protected_len as usize;
    for x in &mut cut[p - 3000..p] {
        *x ^= 0xff;
    }
    fs::write(&arc, &cut).unwrap();
    let r = recovery::repair(&arc, None, &mut noop).unwrap();
    assert!(r.repairable, "{r:?}");
    let a = Archive::open(&r.output[0]).unwrap();
    assert_eq!(extract_all(&a, &dir.path().join("out")), snapshot(&root));
}

#[test]
fn garbage_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("a.nax");
    create(&arc, &[root], &CreateOptions { recovery_percent: 3.0, ..opts() }, &mut noop).unwrap();
    let good = fs::read(&arc).unwrap();
    let mut x = 0x9e3779b97f4a7c15u64;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for round in 0..120 {
        let mut b = good.clone();
        match round % 4 {
            0 => b.truncate((rnd() as usize) % b.len()),
            1 => {
                for _ in 0..1 + rnd() % 8 {
                    let i = (rnd() as usize) % b.len();
                    b[i] = rnd() as u8;
                }
            }
            2 => {
                let i = (rnd() as usize) % b.len();
                b.insert(i, rnd() as u8);
            }
            _ => {
                let i = (rnd() as usize) % (b.len() - 64);
                for k in 0..64 {
                    b[i + k] = rnd() as u8;
                }
            }
        }
        fs::write(&arc, &b).unwrap();
        let out = dir.path().join(format!("o{round}"));
        if let Ok(a) = Archive::open(&arc) {
            let _ = a.extract(&out, &ExtractOptions::default(), &mut noop);
        }
        let _ = recovery::check(&arc, None, &mut noop);
    }
}

#[cfg(windows)]
#[test]
fn planted_junction_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = tree(dir.path());
    let arc = dir.path().join("a.nax");
    create(&arc, &[root], &opts(), &mut noop).unwrap();

    // dest\secret stuff\photos -> outside (a junction, no admin rights needed).
    let dest = dir.path().join("dest");
    let outside = dir.path().join("outside");
    fs::create_dir_all(dest.join("secret stuff")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let st = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dest.join("secret stuff").join("photos"))
        .arg(&outside)
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));

    let a = Archive::open(&arc).unwrap();
    let r = a.extract(&dest, &ExtractOptions { overwrite: nax_core::Overwrite::Replace, ..Default::default() }, &mut noop);
    assert!(matches!(r, Err(Error::UnsafePath(_))), "{r:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "nothing may be written outside");
}