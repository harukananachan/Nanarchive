//! Password encryption: Argon2id turns the password into a key, AES-256-GCM
//! encrypts every block and the file list.
//!
//! - Each archive gets a random 16-byte salt, so the same password gives a
//!   different key for every archive (and nonces never repeat under one key).
//! - The nonce is (purpose, block number): blocks can't be swapped or reused.
//! - The header (format version, flags, KDF settings, salt) is the associated
//!   data of every encryption, so it can't be altered without detection.
//! - A 16-byte check value tells "wrong password" apart from "damaged" without
//!   decrypting anything. Guessing still costs one full Argon2id run per try.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use zeroize::Zeroizing;

use crate::{Error, Result};

/// Size of the encryption header that follows the 16-byte archive header.
pub const CRYPT_HEADER_LEN: u64 = 64;
/// GCM authentication tag appended to every encrypted piece.
pub const TAG_LEN: usize = 16;

const KDF_ARGON2ID: u8 = 1;
const CIPHER_AES256GCM: u8 = 1;

/// Default Argon2id cost: 128 MiB of memory, 3 passes, 4 lanes.
/// About half a second on a desktop CPU; every password guess costs the same.
pub const DEFAULT_M_KIB: u32 = 128 * 1024;
pub const DEFAULT_T: u32 = 3;
pub const DEFAULT_P: u8 = 4;

// Limits for settings read from an archive, so a hostile file can't make us
// allocate huge memory or spin for minutes before saying "wrong password".
const MAX_M_KIB: u32 = 2 * 1024 * 1024;
const MAX_T: u32 = 32;
const MAX_P: u8 = 64;

const NONCE_BLOCK: u32 = 1;
const NONCE_INDEX: u32 = 2;

/// Key derivation settings stored in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u8,
}

impl Default for KdfParams {
    fn default() -> Self {
        KdfParams { m_kib: DEFAULT_M_KIB, t: DEFAULT_T, p: DEFAULT_P }
    }
}

/// The 64-byte encryption header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptHeader {
    pub kdf: KdfParams,
    pub salt: [u8; 16],
    pub check: [u8; 16],
}

impl CryptHeader {
    pub fn to_bytes(&self) -> [u8; CRYPT_HEADER_LEN as usize] {
        let mut b = [0u8; CRYPT_HEADER_LEN as usize];
        b[0] = KDF_ARGON2ID;
        b[1] = CIPHER_AES256GCM;
        b[2] = self.kdf.p;
        b[4..8].copy_from_slice(&self.kdf.m_kib.to_le_bytes());
        b[8..12].copy_from_slice(&self.kdf.t.to_le_bytes());
        b[12..28].copy_from_slice(&self.salt);
        b[28..44].copy_from_slice(&self.check);
        b
    }

    pub fn parse(b: &[u8]) -> Result<CryptHeader> {
        if b.len() != CRYPT_HEADER_LEN as usize {
            return Err(Error::Corrupt("encryption header missing".into()));
        }
        if b[0] != KDF_ARGON2ID || b[1] != CIPHER_AES256GCM {
            return Err(Error::Corrupt("unknown encryption method (made by a newer Nanarchive?)".into()));
        }
        if b[3] != 0 || b[44..].iter().any(|&x| x != 0) {
            return Err(Error::Corrupt("encryption header has unknown fields".into()));
        }
        let kdf = KdfParams {
            p: b[2],
            m_kib: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            t: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        };
        // Argon2 needs at least 8 KiB per lane.
        if kdf.p == 0 || kdf.p > MAX_P || kdf.t == 0 || kdf.t > MAX_T || kdf.m_kib > MAX_M_KIB || kdf.m_kib < 8 * kdf.p as u32 {
            return Err(Error::Corrupt("encryption settings out of range".into()));
        }
        Ok(CryptHeader { kdf, salt: b[12..28].try_into().unwrap(), check: b[28..44].try_into().unwrap() })
    }
}

/// Keys for one archive. The key material is wiped from memory on drop.
pub struct Cipher {
    aead: Aes256Gcm,
    /// Header bytes authenticated with every piece.
    aad: Vec<u8>,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher(..)")
    }
}

/// Password -> (data key, check value).
fn derive(password: &str, kdf: KdfParams, salt: &[u8; 16]) -> Result<(Zeroizing<[u8; 32]>, [u8; 16])> {
    if password.is_empty() {
        return Err(Error::InvalidInput("the password is empty".into()));
    }
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p as u32, Some(32))
        .map_err(|e| Error::Corrupt(format!("encryption settings: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut master = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(password.as_bytes(), salt, master.as_mut())
        .map_err(|e| Error::Codec(format!("key derivation: {e}")))?;
    let key = Zeroizing::new(blake3::derive_key("Nanarchive 2026-11 data key", master.as_ref()));
    let check_full = blake3::derive_key("Nanarchive 2026-11 password check", master.as_ref());
    Ok((key, check_full[..16].try_into().unwrap()))
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Cipher {
    /// New keys for a new archive (random salt). Returns the header to store.
    pub fn create(password: &str, kdf: KdfParams) -> Result<(Cipher, CryptHeader)> {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|e| Error::Codec(format!("no secure random numbers: {e}")))?;
        let (key, check) = derive(password, kdf, &salt)?;
        let header = CryptHeader { kdf, salt, check };
        Ok((Cipher::from_key(&key), header))
    }

    /// Keys for an existing archive; fails with `WrongPassword` if it doesn't match.
    pub fn open(password: &str, header: &CryptHeader) -> Result<Cipher> {
        let (key, check) = derive(password, header.kdf, &header.salt)?;
        if !same(&check, &header.check) {
            return Err(Error::WrongPassword);
        }
        Ok(Cipher::from_key(&key))
    }

    fn from_key(key: &[u8; 32]) -> Cipher {
        Cipher { aead: Aes256Gcm::new(key.into()), aad: Vec::new() }
    }

    /// Sets the archive header bytes every piece is bound to.
    pub fn bind(&mut self, header_bytes: &[u8]) {
        self.aad = header_bytes.to_vec();
    }

    fn nonce(purpose: u32, counter: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&purpose.to_le_bytes());
        n[4..].copy_from_slice(&counter.to_le_bytes());
        n
    }

    fn seal(&self, nonce: [u8; 12], buf: &mut Vec<u8>) -> Result<()> {
        let tag = self
            .aead
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), &self.aad, buf)
            .map_err(|_| Error::Codec("encryption failed".into()))?;
        buf.extend_from_slice(&tag);
        Ok(())
    }

    fn open_piece(&self, nonce: [u8; 12], mut buf: Vec<u8>, what: &str) -> Result<Vec<u8>> {
        if buf.len() < TAG_LEN {
            return Err(Error::Corrupt(format!("{what} is too short")));
        }
        let at = buf.len() - TAG_LEN;
        let tag = Tag::clone_from_slice(&buf[at..]);
        buf.truncate(at);
        self.aead
            .decrypt_in_place_detached(Nonce::from_slice(&nonce), &self.aad, &mut buf, &tag)
            .map_err(|_| Error::Corrupt(format!("{what} failed its authenticity check (damaged or altered)")))?;
        Ok(buf)
    }

    pub fn seal_block(&self, no: u64, buf: &mut Vec<u8>) -> Result<()> {
        self.seal(Self::nonce(NONCE_BLOCK, no), buf)
    }

    pub fn open_block(&self, no: u64, buf: Vec<u8>) -> Result<Vec<u8>> {
        self.open_piece(Self::nonce(NONCE_BLOCK, no), buf, &format!("block {no}"))
    }

    pub fn seal_index(&self, buf: &mut Vec<u8>) -> Result<()> {
        self.seal(Self::nonce(NONCE_INDEX, 0), buf)
    }

    pub fn open_index(&self, buf: Vec<u8>) -> Result<Vec<u8>> {
        self.open_piece(Self::nonce(NONCE_INDEX, 0), buf, "file list")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cheap settings so tests run fast.
    const FAST: KdfParams = KdfParams { m_kib: 64, t: 1, p: 1 };

    #[test]
    fn seal_and_open() {
        let (mut c, h) = Cipher::create("correct horse", FAST).unwrap();
        c.bind(b"header");
        let mut buf = b"secret data".to_vec();
        c.seal_block(7, &mut buf).unwrap();
        assert_ne!(&buf[..11], b"secret data");

        let mut c2 = Cipher::open("correct horse", &CryptHeader::parse(&h.to_bytes()).unwrap()).unwrap();
        c2.bind(b"header");
        assert_eq!(c2.open_block(7, buf.clone()).unwrap(), b"secret data");
        // Wrong block number, altered byte, or altered header all fail.
        assert!(c2.open_block(8, buf.clone()).is_err());
        let mut bad = buf.clone();
        bad[3] ^= 1;
        assert!(c2.open_block(7, bad).is_err());
        c2.bind(b"headeR");
        assert!(c2.open_block(7, buf).is_err());
    }

    #[test]
    fn wrong_password_is_detected() {
        let (_, h) = Cipher::create("one", FAST).unwrap();
        assert!(matches!(Cipher::open("two", &h), Err(Error::WrongPassword)));
        assert!(Cipher::open("one", &h).is_ok());
    }

    #[test]
    fn salts_differ() {
        let (_, a) = Cipher::create("pw", FAST).unwrap();
        let (_, b) = Cipher::create("pw", FAST).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.check, b.check);
    }

    #[test]
    fn hostile_settings_are_refused() {
        let (_, h) = Cipher::create("pw", FAST).unwrap();
        let mut b = h.to_bytes();
        b[4..8].copy_from_slice(&u32::MAX.to_le_bytes()); // 4 TiB of memory
        assert!(CryptHeader::parse(&b).is_err());
        let mut b = h.to_bytes();
        b[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(CryptHeader::parse(&b).is_err());
    }
}
