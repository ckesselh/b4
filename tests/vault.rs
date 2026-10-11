//! The container reader against the fixtures produced with Banking4 in
//! `tests/fixtures/container` (see the README there) and against damaged
//! copies of them.
//!
//! Section numbers refer to `docs/format/01-container.md`.

// Everything in this file is test code. The allowance for `unwrap` in tests
// (clippy.toml) covers `#[test]` functions only, not the helpers they share.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use b4::vault::{
    find_entry, ChainFault, Cipher, Entry, Guid, Header, TableFault, Vault, VaultError,
    HEADER_SIZE, MAX_KDF_ROUNDS,
};
use serde_json::Value;

// --- Fixtures and the manifest ---------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container")
}

/// The `vaults` array of `MANIFEST.json`.
fn manifest() -> Vec<Value> {
    let text = std::fs::read(fixture_dir().join("MANIFEST.json")).unwrap();
    let manifest: Value = serde_json::from_slice(&text).unwrap();
    manifest["vaults"].as_array().unwrap().clone()
}

/// The manifest record of the fixture whose file name starts with `prefix`
/// (`"v02"`).
fn fixture(prefix: &str) -> Value {
    manifest()
        .into_iter()
        .find(|v| v["file"].as_str().unwrap().starts_with(prefix))
        .unwrap()
}

fn fixture_path(vault: &Value) -> PathBuf {
    fixture_dir().join(vault["file"].as_str().unwrap())
}

fn fixture_bytes(vault: &Value) -> Vec<u8> {
    std::fs::read(fixture_path(vault)).unwrap()
}

fn password(vault: &Value) -> &str {
    vault["password"].as_str().unwrap()
}

fn device_guid(vault: &Value) -> Option<Guid> {
    vault["device_guid"].as_str().map(|g| g.parse().unwrap())
}

/// Unlock with what the manifest gives: the password and, for a device-bound
/// fixture, the device GUID.
fn unlock(vault: &mut Vault, record: &Value) -> Result<(), VaultError> {
    match device_guid(record) {
        Some(guid) if vault.header().is_device_bound() => {
            vault.unlock_with_device_guid(password(record), &guid)
        }
        _ => vault.unlock(password(record)),
    }
}

fn open_unlocked(bytes: Vec<u8>, record: &Value) -> Result<Vault, VaultError> {
    let mut vault = Vault::from_bytes(bytes)?;
    unlock(&mut vault, record)?;
    Ok(vault)
}

// --- Small tools --------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_digit(16).unwrap() as u8)
        .collect();
    assert_eq!(digits.len() % 2, 0);
    digits
        .chunks(2)
        .map(|pair| pair[0] << 4 | pair[1])
        .collect()
}

/// SHA-256 (FIPS 180-4), for the digests of the manifest and of 01 §12. The
/// crate has no SHA-2 dependency; `sha256_matches_the_published_vectors` pins
/// this implementation.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[test]
fn sha256_matches_the_published_vectors() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        sha256(&[b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

/// The generated contents of a fixture file (fixtures README, "File
/// contents"): the top byte of a linear congruential generator seeded with the
/// entry's `seed`.
fn generated(seed: u32, size: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..size)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// A deterministic pseudo-random generator (xorshift64*), so that every run
/// damages the same bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

// --- Walking a vault ------------------------------------------------------------

/// One entry of a tree walk: the path as Banking4 lists it (names joined
/// with `/`) and the entry.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    path: String,
    entry: Entry,
}

/// Why a strict walk stopped.
#[derive(Debug)]
enum Failure {
    Vault(VaultError),
}

impl From<VaultError> for Failure {
    fn from(error: VaultError) -> Self {
        Self::Vault(error)
    }
}

/// List the whole tree depth first, a directory before its contents: the
/// order of the manifest. Reads directory pages only.
///
/// The walk keeps no record of where it has been. A directory that leads
/// back to one of its ancestors (01 §7.4) is refused by the vault itself,
/// because its pages belong to the entry they were first read for.
fn walk(vault: &Vault) -> Result<Vec<Node>, Failure> {
    fn into(
        vault: &Vault,
        entries: &[Entry],
        prefix: &str,
        out: &mut Vec<Node>,
    ) -> Result<(), Failure> {
        for entry in entries {
            let path = format!("{prefix}{}", entry.name());
            out.push(Node {
                path: path.clone(),
                entry: entry.clone(),
            });
            if entry.is_dir() {
                let children = vault.read_dir(entry)?;
                into(vault, &children, &format!("{path}/"), out)?;
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    into(vault, vault.root(), "", &mut out)?;
    Ok(out)
}

/// A whole tree: every entry, and for a file its contents.
type Tree = Vec<(Node, Option<Vec<u8>>)>;

/// The whole tree with the contents of every file.
fn snapshot(vault: &Vault) -> Result<Tree, Failure> {
    walk(vault)?
        .into_iter()
        .map(|node| {
            let contents = if node.entry.is_dir() {
                None
            } else {
                Some(vault.read_file(&node.entry)?)
            };
            Ok((node, contents))
        })
        .collect()
}

/// Resolve `path` (names joined with `/`) entry by entry (01 §7.3).
fn lookup(vault: &Vault, path: &str) -> Option<Entry> {
    let mut entries = vault.root().to_vec();
    let mut names = path.split('/').peekable();
    while let Some(name) = names.next() {
        let entry = find_entry(&entries, name)?.clone();
        if names.peek().is_none() {
            return Some(entry);
        }
        entries = vault.read_dir(&entry).ok()?;
    }
    None
}

fn read(vault: &Vault, path: &str) -> Result<Vec<u8>, VaultError> {
    vault.read_file(&lookup(vault, path).expect("the entry is listed"))
}

// --- The manifest -----------------------------------------------------------------

#[test]
fn headers_match_the_manifest() {
    let vaults = manifest();
    assert_eq!(vaults.len(), 9);
    for record in &vaults {
        let file = record["file"].as_str().unwrap();
        let bytes = fixture_bytes(record);
        // A manifest that belongs to other files would not describe these.
        assert_eq!(bytes.len() as u64, record["file_size"], "{file}");
        assert_eq!(
            sha256(&bytes),
            record["file_sha256"].as_str().unwrap(),
            "{file}"
        );

        let header = Header::read(fixture_path(record)).unwrap();
        assert_eq!(header, Header::parse(&bytes).unwrap(), "{file}");
        let expected = &record["header"];
        assert_eq!(header.format_version(), 1, "{file}");
        assert_eq!(header.cluster_size(), expected["cluster_size"], "{file}");
        assert_eq!(
            header.raw_cluster_size(),
            expected["cluster_size"],
            "{file}"
        );
        assert_eq!(header.created_at().raw(), expected["created_at"], "{file}");
        assert_eq!(
            header.modified_at().raw(),
            expected["modified_at"],
            "{file}"
        );
        assert_eq!(
            header.security_flags(),
            expected["security_flags"],
            "{file}"
        );
        assert_eq!(
            header.is_device_bound(),
            expected["security_flags"].as_u64().unwrap() & 1 == 1,
            "{file}"
        );
        assert_eq!(header.cipher_id(), expected["cipher_id"], "{file}");
        assert_eq!(
            header.cipher(),
            Cipher::from_id(expected["cipher_id"].as_i64().unwrap() as i32),
            "{file}"
        );
        assert!(header.cipher().is_some(), "{file}");
        assert_eq!(header.kdf_rounds(), expected["kdf_rounds"], "{file}");
        assert_eq!(
            header.effective_kdf_rounds(),
            expected["kdf_rounds"],
            "{file}"
        );
        assert_eq!(
            header.file_guid().to_string(),
            expected["file_guid"].as_str().unwrap(),
            "{file}"
        );
        assert_eq!(
            header.type_guid().to_string(),
            expected["type_guid"].as_str().unwrap(),
            "{file}"
        );
        assert_eq!(
            header.failed_attempts(),
            expected["failed_attempts"],
            "{file}"
        );
        assert_eq!(header.has_access_token(), expected["has_token"], "{file}");
        assert_eq!(header.root_dir_cluster(), 1, "{file}");

        // 01 §3.4: the stamps are UTC seconds since 2000 and both are set.
        for stamp in [header.created_at(), header.modified_at()] {
            assert!(stamp.is_set(), "{file}");
            assert_eq!(
                stamp.unix_seconds(),
                Some(946_684_800 + i64::from(stamp.raw())),
                "{file}"
            );
            let text = stamp.to_iso8601().unwrap();
            assert_eq!(text.len(), 20, "{file}");
            assert!(text.ends_with('Z'), "{file}");
        }
        assert!(header.created_at() <= header.modified_at(), "{file}");

        // 01 §13.1: what nothing uses is zero in files Banking4 wrote.
        assert_eq!(header.legacy_area(), &[0u8; 20], "{file}");
        assert_eq!(header.reserved(), &[0u8; 40], "{file}");
        let meaningful = header.cipher().unwrap().key_len().div_ceil(16) * 16;
        assert!(
            header.wrapped_key()[meaningful..].iter().all(|b| *b == 0),
            "{file}"
        );
    }
}

#[test]
fn trees_and_contents_match_the_manifest() {
    let mut entries = 0;
    let mut files = 0;
    let mut empty_files = 0;
    let mut directories = 0;
    for record in manifest() {
        let file = record["file"].as_str().unwrap();
        let mut vault = Vault::open(fixture_path(&record)).unwrap();
        assert_eq!(vault.file_len(), record["file_size"], "{file}");
        unlock(&mut vault, &record).unwrap();
        assert!(vault.is_unlocked());

        let tree = snapshot(&vault).unwrap();
        let expected = record["entries"].as_array().unwrap();
        assert_eq!(tree.len(), expected.len(), "{file}");
        for ((node, contents), want) in tree.iter().zip(expected) {
            let path = want["path"].as_str().unwrap();
            assert_eq!(node.path, path, "{file}");
            assert_eq!(
                hex(node.entry.name_bytes()),
                want["name_utf8"].as_str().unwrap(),
                "{file}: {path}"
            );
            assert_eq!(node.entry.size(), want["size"], "{file}: {path}");
            assert_eq!(node.entry.flags(), want["flags"], "{file}: {path}");
            assert_eq!(
                node.entry.is_encrypted(),
                want["encrypted"],
                "{file}: {path}"
            );
            entries += 1;
            match want["kind"].as_str().unwrap() {
                "dir" => {
                    assert!(node.entry.is_dir(), "{file}: {path}");
                    assert!(contents.is_none());
                    directories += 1;
                }
                "file" => {
                    assert!(!node.entry.is_dir(), "{file}: {path}");
                    let contents = contents.as_ref().unwrap();
                    let seed = want["seed"].as_u64().unwrap() as u32;
                    assert!(
                        *contents == generated(seed, node.entry.size() as usize),
                        "{file}: {path}: contents differ from the generator"
                    );
                    assert_eq!(
                        sha256(contents),
                        want["sha256"].as_str().unwrap(),
                        "{file}: {path}"
                    );
                    files += 1;
                    if contents.is_empty() {
                        empty_files += 1;
                    }
                }
                other => panic!("{file}: unknown kind {other}"),
            }
        }

        // 01 §11: reading leaves the file as it was.
        assert_eq!(
            sha256(&fixture_bytes(&record)),
            record["file_sha256"].as_str().unwrap(),
            "{file}"
        );
    }
    // The totals of 01 §13.1.
    assert_eq!(
        (entries, files, empty_files, directories),
        (464, 410, 146, 54)
    );
}

#[test]
fn the_fixtures_cover_what_their_names_promise() {
    // Cluster sizes, ciphers and round counts of 01 §13.1, so that a changed
    // fixture set does not silently test less.
    let seen: Vec<(u32, Cipher, i32, u32)> = manifest()
        .iter()
        .map(|record| {
            let header = Header::read(fixture_path(record)).unwrap();
            (
                header.cluster_size(),
                header.cipher().unwrap(),
                header.kdf_rounds(),
                header.security_flags(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (512, Cipher::Aes256, 100_000, 0),
            (512, Cipher::Aes256, 0, 0),
            (512, Cipher::Aes256, 0, 0),
            (4096, Cipher::Aes128, 1000, 0),
            (1024, Cipher::Aes192, 3, 0),
            (512, Cipher::Aes256, 100_000, 2),
            (512, Cipher::Aes256, 10, 1),
            (512, Cipher::Aes256, 10, 0),
            (512, Cipher::Aes256, 50_000, 2),
        ]
    );
}

#[test]
fn listing_needs_no_password_and_plain_files_read_while_locked() {
    // 01 §9: "Steps 1 to 6 and reading unencrypted files need no password."
    for record in manifest() {
        let file = record["file"].as_str().unwrap();
        let mut vault = Vault::open(fixture_path(&record)).unwrap();
        assert!(!vault.is_unlocked());
        let locked_tree = walk(&vault).unwrap();
        assert_eq!(
            locked_tree.len(),
            record["entries"].as_array().unwrap().len()
        );
        let mut plain = Vec::new();
        for node in locked_tree.iter().filter(|node| !node.entry.is_dir()) {
            if node.entry.is_encrypted() {
                assert!(
                    matches!(vault.read_file(&node.entry), Err(VaultError::Locked)),
                    "{file}: {}",
                    node.path
                );
            } else {
                plain.push(vault.read_file(&node.entry).unwrap());
            }
        }
        assert!(!plain.is_empty(), "{file}");

        unlock(&mut vault, &record).unwrap();
        assert_eq!(walk(&vault).unwrap(), locked_tree, "{file}");
        let plain_again: Vec<Vec<u8>> = locked_tree
            .iter()
            .filter(|node| !node.entry.is_dir() && !node.entry.is_encrypted())
            .map(|node| vault.read_file(&node.entry).unwrap())
            .collect();
        assert_eq!(plain, plain_again, "{file}");
    }
}

// --- Passwords -----------------------------------------------------------------------

#[test]
fn wrong_passwords_are_rejected() {
    // 01 §13.1: the password with a character appended, in another case, with
    // a leading space, and the empty password (a non-empty one for v03).
    for record in manifest() {
        let file = record["file"].as_str().unwrap();
        let right = password(&record);
        let mut wrong = vec![
            format!("{right}x"),
            format!(" {right}"),
            format!("{right} "),
            right.to_uppercase(),
            if right.is_empty() {
                "x".to_string()
            } else {
                String::new()
            },
        ];
        wrong.retain(|candidate| candidate != right);
        if let Some(first) = record["created_with"]["password"].as_str() {
            // v09: the password the container was created with.
            wrong.push(first.to_string());
        }
        assert!(wrong.len() >= 4, "{file}");

        let mut vault = Vault::open(fixture_path(&record)).unwrap();
        let guid = device_guid(&record);
        for candidate in &wrong {
            let result = match &guid {
                Some(guid) => vault.unlock_with_device_guid(candidate, guid),
                None => vault.unlock(candidate),
            };
            assert!(
                matches!(result, Err(VaultError::WrongPassword)),
                "{file}: a wrong password was not rejected"
            );
            assert!(!vault.is_unlocked(), "{file}");
        }
        // The right one still works afterwards.
        unlock(&mut vault, &record).unwrap();
        assert!(vault.is_unlocked(), "{file}");
    }
}

#[test]
fn a_failed_unlock_keeps_an_unlocked_vault_unlocked() {
    let record = fixture("v02");
    let mut vault = open_unlocked(fixture_bytes(&record), &record).unwrap();
    let before = read(&vault, "b17.enc").unwrap();
    assert!(matches!(
        vault.unlock("not it"),
        Err(VaultError::WrongPassword)
    ));
    assert!(vault.is_unlocked());
    assert_eq!(read(&vault, "b17.enc").unwrap(), before);
}

#[test]
fn the_empty_password_opens_v03() {
    // 01 §5.5.
    let record = fixture("v03");
    assert_eq!(password(&record), "");
    let mut vault = Vault::open(fixture_path(&record)).unwrap();
    assert_eq!(vault.header().kdf_rounds(), 0);
    assert_eq!(record["requested"]["kdf_rounds"], 100_000);
    vault.unlock("").unwrap();
    assert!(snapshot(&vault).is_ok());
}

#[test]
fn a_non_bmp_password_opens_v05() {
    // 01 §5.1: UTF-16LE with a surrogate pair, no normalisation.
    let record = fixture("v05");
    let right = password(&record);
    assert!(right.chars().any(|c| c as u32 > 0xFFFF));
    let mut vault = Vault::open(fixture_path(&record)).unwrap();
    // "ä" as "a" + combining diaeresis is a different password.
    let decomposed = right.replace('\u{e4}', "a\u{308}");
    assert_ne!(decomposed, right);
    assert!(matches!(
        vault.unlock(&decomposed),
        Err(VaultError::WrongPassword)
    ));
    vault.unlock(right).unwrap();
}

#[test]
fn the_device_bound_fixture_needs_its_device_guid() {
    // 01 §3.6, §5.6: v07 has security_flags 1.
    let record = fixture("v07");
    let guid = device_guid(&record).unwrap();
    let mut vault = Vault::open(fixture_path(&record)).unwrap();
    assert!(vault.header().is_device_bound());

    // What b4 0.1.0 does: it has no device GUID to offer.
    let error = vault.unlock(password(&record)).unwrap_err();
    assert!(matches!(error, VaultError::DeviceBound));
    assert!(error
        .to_string()
        .contains("cannot open device-bound vaults"));
    assert!(!vault.is_unlocked());
    // The structure is readable all the same.
    assert_eq!(
        walk(&vault).unwrap().len(),
        record["entries"].as_array().unwrap().len()
    );

    // A missing, empty or different GUID is rejected like a wrong password.
    let mut other = *guid.as_bytes();
    other[0] ^= 1;
    for wrong in [Guid::from_bytes([0; 16]), Guid::from_bytes(other)] {
        assert!(matches!(
            vault.unlock_with_device_guid(password(&record), &wrong),
            Err(VaultError::WrongPassword)
        ));
    }
    // The GUID hashed in its textual byte order is wrong as well.
    let textual: Vec<u8> = unhex(&guid.to_string().replace('-', ""));
    assert!(matches!(
        vault.unlock_with_device_guid(
            password(&record),
            &Guid::from_bytes(textual.try_into().unwrap())
        ),
        Err(VaultError::WrongPassword)
    ));

    vault
        .unlock_with_device_guid(password(&record), &guid)
        .unwrap();
    assert!(snapshot(&vault).is_ok());
}

#[test]
fn flags_value_2_involves_no_device_guid() {
    // 01 §3.6: v06 was created while a device GUID was supplied, v09 got its
    // flags with a password change. Neither is device-bound.
    for prefix in ["v06", "v09"] {
        let record = fixture(prefix);
        let mut vault = Vault::open(fixture_path(&record)).unwrap();
        assert_eq!(vault.header().security_flags(), 2);
        assert!(!vault.header().is_device_bound());
        vault.unlock(password(&record)).unwrap();

        // A GUID that is offered anyway makes no difference.
        let mut again = Vault::open(fixture_path(&record)).unwrap();
        again
            .unlock_with_device_guid(password(&record), &Guid::from_bytes([0x5A; 16]))
            .unwrap();
        assert_eq!(read(&vault, "b17.enc").ok(), read(&again, "b17.enc").ok());
    }
}

#[test]
fn a_changed_password_keeps_the_contents() {
    // 01 §5.4: v09 was created with another password, round count and flags.
    let record = fixture("v09");
    let created = &record["created_with"];
    assert_ne!(created["password"], record["password"]);
    assert_ne!(created["kdf_rounds"], record["header"]["kdf_rounds"]);
    let mut vault = Vault::open(fixture_path(&record)).unwrap();
    assert!(matches!(
        vault.unlock(created["password"].as_str().unwrap()),
        Err(VaultError::WrongPassword)
    ));
    vault.unlock(password(&record)).unwrap();
    let tree = snapshot(&vault).unwrap();
    assert_eq!(tree.len(), record["entries"].as_array().unwrap().len());
}

#[test]
fn an_issued_access_token_changes_nothing_for_the_password() {
    // 01 §5.6: v08 has token_salt and token_check set.
    let record = fixture("v08");
    let mut vault = Vault::open(fixture_path(&record)).unwrap();
    assert!(vault.header().has_access_token());
    assert_ne!(vault.header().token_check(), &[0u8; 32]);
    vault.unlock(password(&record)).unwrap();
    assert!(snapshot(&vault).is_ok());
}

// --- The pass hash against an independent RIPEMD-160 ----------------------------------

/// The constant that starts every pass hash (01 §5.1).
const PASS_CONSTANT: [u8; 32] = [
    0x90, 0x3c, 0x4a, 0x16, 0x40, 0xb4, 0x43, 0x81, 0xb1, 0x99, 0x86, 0x6a, 0xa4, 0x51, 0x6d, 0x19,
    0x0c, 0xb0, 0xdc, 0xb8, 0xef, 0xa1, 0x4c, 0x1a, 0x97, 0xce, 0x04, 0x3b, 0x34, 0x1a, 0x34, 0xcf,
];

/// The smallest container that `password` opens: a header, an allocation
/// table and an empty root directory. Built as a writer would build it
/// (01 §5.1 to §5.4), with the RIPEMD-160 of the RustCrypto `ripemd` crate.
/// Returns the image and the number of bytes that went through the hash.
fn container_for(password: &str, device: Option<&Guid>, rounds: i32) -> (Vec<u8>, usize) {
    use ripemd::Digest;

    let mut message = PASS_CONSTANT.to_vec();
    message.extend(password.encode_utf16().flat_map(u16::to_le_bytes));
    if let Some(guid) = device {
        message.extend(guid.as_bytes());
    }
    let digest = ripemd::Ripemd160::digest(&message);
    let mut pass_key = [0u8; 32];
    let mut running = 0u8;
    for (out, d) in pass_key.iter_mut().zip(digest.iter().cycle()) {
        running ^= d;
        *out = running;
    }
    let salt: [u8; 32] = std::array::from_fn(|i| (i * 13 + message.len()) as u8);
    for _ in 0..rounds {
        let x = encrypt(&pass_key, &salt);
        for (k, x) in pass_key.iter_mut().zip(x) {
            *k ^= x;
        }
    }
    let content_key: [u8; 32] = std::array::from_fn(|i| 0xC0 ^ i as u8);

    let mut image = vec![0u8; 3 * 512];
    image[..4].copy_from_slice(&unhex("53 55 42 01"));
    image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
    image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
    image[0x10..0x14].copy_from_slice(&i32::from(device.is_some()).to_le_bytes());
    image[0x14..0x18].copy_from_slice(&3i32.to_le_bytes());
    image[0x18..0x1C].copy_from_slice(&rounds.to_le_bytes());
    image[0x1C..0x3C].copy_from_slice(&salt);
    image[0x3C..0x5C].copy_from_slice(&encrypt(&pass_key, &salt));
    image[0x100..0x120].copy_from_slice(&encrypt(&pass_key, &content_key));
    image[512..520].copy_from_slice(&unhex("ff 00 00 00  ff 00 00 00"));
    (image, message.len())
}

#[test]
fn the_pass_hash_agrees_with_an_independent_ripemd160() {
    // 01 §5.1. b4 computes RIPEMD-160 itself, so that the state the
    // password goes through can be wiped. Here every container is locked
    // with the RustCrypto implementation and must open with b4's, for
    // passwords whose hashed bytes end before, on and after the block
    // boundaries of the hash (55/56 bytes of a block are the limit for the
    // padding, 64 bytes are a block).
    let device: Guid = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
    let mut hashed = HashSet::new();
    let alphabet: Vec<char> = "aZ0 \u{e4}\u{20ac}\u{1f600}-".chars().collect();
    for len in 0..=70usize {
        let password: String = (0..len).map(|i| alphabet[(i * 5 + len) % 8]).collect();
        for device in [None, Some(&device)] {
            let (image, message_len) = container_for(&password, device, 2);
            hashed.insert(message_len);
            let mut vault = Vault::from_bytes(image).unwrap();
            let unlock = |vault: &mut Vault, password: &str| match device {
                Some(guid) => vault.unlock_with_device_guid(password, guid),
                None => vault.unlock(password),
            };
            assert!(
                matches!(
                    unlock(&mut vault, &format!("{password}x")),
                    Err(VaultError::WrongPassword)
                ),
                "{len} characters"
            );
            assert!(!vault.is_unlocked());
            unlock(&mut vault, &password).unwrap_or_else(|error| {
                panic!("{len} characters, device {}: {error}", device.is_some())
            });
            assert!(vault.is_unlocked());
        }
    }
    // The message is the 32-byte constant, two bytes per UTF-16 unit and
    // perhaps 16 bytes of GUID, so its length is always even: the lengths
    // next to and on the boundaries that can occur did occur.
    for boundary in [32, 54, 56, 62, 64, 66, 118, 120, 126, 128, 130] {
        assert!(hashed.contains(&boundary), "{boundary} bytes");
    }
}

// --- The worked example of 01 §12 ---------------------------------------------------

/// AES-256-ECB encryption with zero fill to a block boundary (01 §4), to
/// build the example the way a writer would.
fn encrypt(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes256Enc::new(GenericArray::from_slice(key));
    let mut out = plain.to_vec();
    out.resize(plain.len().div_ceil(16) * 16, 0);
    for block in out.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

fn slot(first_cluster: i32, size: i32, flags: u8, name: &[u8]) -> [u8; 64] {
    let mut slot = [0u8; 64];
    slot[0..4].copy_from_slice(&first_cluster.to_le_bytes());
    slot[4..8].copy_from_slice(&size.to_le_bytes());
    slot[8] = flags;
    slot[9] = name.len() as u8;
    slot[10..10 + name.len()].copy_from_slice(name);
    slot
}

fn two_bin() -> Vec<u8> {
    (0..520usize).map(|i| ((7 * i + 3) % 256) as u8).collect()
}

/// The 4096-byte container of 01 §12, built from the values printed there.
fn worked_example() -> Vec<u8> {
    let content_key: [u8; 32] = std::array::from_fn(|i| 0xa0 + i as u8);
    let cluster = |bytes: &[u8]| {
        let mut cluster = bytes.to_vec();
        cluster.resize(512, 0);
        cluster
    };

    // 12.2
    let mut header = vec![0u8; 512];
    header[0x000..0x004].copy_from_slice(&unhex("53 55 42 01"));
    header[0x004..0x008].copy_from_slice(&unhex("00 02 00 00"));
    header[0x008..0x00C].copy_from_slice(&unhex("80 75 e8 30"));
    header[0x00C..0x010].copy_from_slice(&unhex("01 00 00 00"));
    header[0x014..0x018].copy_from_slice(&unhex("03 00 00 00"));
    header[0x018..0x01C].copy_from_slice(&unhex("02 00 00 00"));
    for (i, byte) in header[0x01C..0x03C].iter_mut().enumerate() {
        *byte = 0x10 + i as u8;
    }
    header[0x03C..0x05C].copy_from_slice(&unhex(
        "65 73 56 35 7a a3 33 06 22 b6 cb 47 01 a8 cc 10
         a7 1d 92 cd be 77 3d 0e d1 a8 4d 52 e5 ed b5 cb",
    ));
    header[0x070..0x080].copy_from_slice(&unhex("67 45 23 01 ab 89 ef cd 01 23 45 67 89 ab cd ef"));
    header[0x080..0x090].copy_from_slice(&unhex("11 11 11 11 22 22 33 33 44 44 55 55 55 55 55 55"));
    header[0x0D4..0x0D8].copy_from_slice(&unhex("90 83 e8 30"));
    header[0x100..0x120].copy_from_slice(&unhex(
        "02 a5 79 19 71 48 02 c4 b9 b2 10 9b 65 a2 42 23
         17 5a b6 8f 4c aa e3 c1 e6 6f bb a1 cb e7 14 e1",
    ));

    // 12.4
    let table = unhex(
        "ff 00 00 00  ff 00 00 00  ff 00 00 00  ff 00 00 00
         ff 00 00 00  01 06 00 00  ff 00 00 00",
    );
    // 12.5
    let root = [
        slot(2, 20, 1, b"hello.txt"),
        slot(3, 5, 0, b"plain.txt"),
        slot(4, 0, 2, b"Dir"),
    ]
    .concat();
    let dir = slot(5, 520, 1, b"two.bin");
    // 12.6
    let two = two_bin();

    [
        header,
        cluster(&table),
        cluster(&root),
        cluster(&encrypt(&content_key, b"Hello, synthetic b4!")),
        cluster(b"plain"),
        cluster(&dir),
        cluster(&encrypt(&content_key, &two[..512])),
        cluster(&encrypt(&content_key, &two[512..])),
    ]
    .concat()
}

#[test]
fn the_worked_example_reads_as_documented() {
    let image = worked_example();
    assert_eq!(image.len(), 4096);
    // The digest the document gives for the whole file, so the image really
    // is the one of 01 §12 ...
    assert_eq!(
        sha256(&image),
        "8c35aa4a62355886fbd739c2338c3f0d2ece07e881a47c2ee9344632106f571a"
    );
    // ... and the ciphertext it prints (12.6).
    assert_eq!(
        hex(&image[0x600..0x620]),
        "c4429701b0d557673a171f83087f652c8d1f058bc20bee8e8c35cca2bc6ad747"
    );
    assert_eq!(
        hex(&image[0xC00..0xC10]),
        "4a8dc246e844694ccf1555b931a23425"
    );
    assert_eq!(
        hex(&image[0xE00..0xE10]),
        "c8445e860e7aa1ac02601c4f200dfbc7"
    );
    assert!(image[0xE10..0x1000].iter().all(|b| *b == 0));

    let mut vault = Vault::from_bytes(image).unwrap();
    assert_eq!(vault.cluster_count(), 7);
    let header = vault.header();
    assert_eq!(header.cluster_size(), 512);
    assert_eq!(
        header.created_at().to_iso8601().unwrap(),
        "2026-01-01T00:00:00Z"
    );
    assert_eq!(
        header.modified_at().to_iso8601().unwrap(),
        "2026-01-01T01:00:00Z"
    );
    assert_eq!(header.cipher(), Some(Cipher::Aes256));
    assert_eq!(header.kdf_rounds(), 2);
    assert_eq!(
        header.file_guid().to_string(),
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_eq!(
        header.type_guid().to_string(),
        "11111111-2222-3333-4444-555555555555"
    );

    assert!(matches!(
        vault.unlock("b4-Example"),
        Err(VaultError::WrongPassword)
    ));
    vault.unlock("b4-example").unwrap();

    let tree = snapshot(&vault).unwrap();
    let listed: Vec<(&str, u8, u32)> = tree
        .iter()
        .map(|(node, _)| (node.path.as_str(), node.entry.flags(), node.entry.size()))
        .collect();
    assert_eq!(
        listed,
        [
            ("hello.txt", 1, 20),
            ("plain.txt", 0, 5),
            ("Dir", 2, 0),
            ("Dir/two.bin", 1, 520),
        ]
    );
    assert_eq!(tree[0].1.as_deref(), Some(&b"Hello, synthetic b4!"[..]));
    assert_eq!(tree[1].1.as_deref(), Some(&b"plain"[..]));
    assert_eq!(tree[2].1, None);
    let two = tree[3].1.as_ref().unwrap();
    assert_eq!(*two, two_bin());
    assert_eq!(
        sha256(two),
        "fd9aea67e2518f689c37aa9bf3305face76a9803d62b813687c4504c1d0cb539"
    );
}

// --- Damaged containers (01 §13.2) -------------------------------------------------

/// A fixture image with what a test needs to find and patch its structures.
/// It follows the fixture's own table and directories, which are intact, and
/// is independent of the reader under test.
#[derive(Clone)]
struct Image {
    bytes: Vec<u8>,
    cluster_size: usize,
}

impl Image {
    fn of(record: &Value) -> Self {
        let bytes = fixture_bytes(record);
        let cluster_size = record["header"]["cluster_size"].as_u64().unwrap() as usize;
        Self {
            bytes,
            cluster_size,
        }
    }

    fn cluster_count(&self) -> usize {
        self.bytes.len() / self.cluster_size - 1
    }

    fn cluster_offset(&self, cluster: usize) -> usize {
        (cluster + 1) * self.cluster_size
    }

    fn cluster_range(&self, cluster: usize) -> Range<usize> {
        self.cluster_offset(cluster)..self.cluster_offset(cluster + 1)
    }

    /// Offset of the allocation table entry of `cluster` (01 §6.2).
    fn table_offset(&self, cluster: usize) -> usize {
        let per_page = self.cluster_size / 4;
        self.cluster_offset(cluster / per_page * per_page) + cluster % per_page * 4
    }

    fn table_pages(&self) -> Vec<usize> {
        (0..self.cluster_count())
            .step_by(self.cluster_size / 4)
            .collect()
    }

    /// The clusters of the chain that starts at `first` (01 §6.3).
    fn chain(&self, first: usize) -> Vec<usize> {
        let mut chain = vec![first];
        loop {
            let at = self.table_offset(*chain.last().unwrap());
            match self.bytes[at] {
                0xFF => return chain,
                0x01 => chain.push(u32::from_le_bytes([
                    self.bytes[at + 1],
                    self.bytes[at + 2],
                    self.bytes[at + 3],
                    0,
                ]) as usize),
                status => panic!("fixture chain with status {status:#x}"),
            }
        }
    }

    fn i32_at(&self, offset: usize) -> i32 {
        i32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }

    fn put_i32(&mut self, offset: usize, value: i32) {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn root_dir(&self) -> usize {
        self.i32_at(0x0C) as usize
    }

    /// Offset of the directory slot of `path` (names joined with `/`).
    fn slot(&self, path: &str) -> usize {
        let mut dir = self.root_dir();
        let mut found = None;
        for name in path.split('/') {
            let offset = self
                .chain(dir)
                .into_iter()
                .flat_map(|page| self.cluster_range(page).step_by(64))
                .find(|&slot| {
                    let len = self.bytes[slot + 9] as usize;
                    self.i32_at(slot) != 0
                        && &self.bytes[slot + 10..slot + 10 + len] == name.as_bytes()
                })
                .unwrap_or_else(|| panic!("no slot for {name}"));
            dir = self.i32_at(offset) as usize;
            found = Some(offset);
        }
        found.unwrap()
    }

    fn first_cluster(&self, path: &str) -> usize {
        self.i32_at(self.slot(path)) as usize
    }

    fn size(&self, path: &str) -> usize {
        self.i32_at(self.slot(path) + 4) as usize
    }

    /// The highest cluster a reader has to look at: table pages, directory
    /// pages and the clusters of every file that is not empty. The cluster
    /// of an empty file is in use but never consulted (01 §8.1).
    fn highest_needed_cluster(&self) -> usize {
        let mut needed = self.table_pages();
        for page in self.directory_pages() {
            needed.push(page);
            for slot in self.cluster_range(page).step_by(64) {
                let first = self.i32_at(slot);
                let is_file = self.bytes[slot + 8] & 2 == 0;
                if first > 0 && is_file && self.i32_at(slot + 4) > 0 {
                    needed.extend(self.chain(first as usize));
                }
            }
        }
        needed.into_iter().max().unwrap()
    }

    /// Every cluster that is a directory page, of every directory.
    fn directory_pages(&self) -> Vec<usize> {
        let mut pages = Vec::new();
        let mut pending = vec![self.root_dir()];
        while let Some(dir) = pending.pop() {
            for page in self.chain(dir) {
                pages.push(page);
                for slot in self.cluster_range(page).step_by(64) {
                    let first = self.i32_at(slot);
                    if first > 0 && self.bytes[slot + 8] & 2 != 0 {
                        pending.push(first as usize);
                    }
                }
            }
        }
        pages
    }
}

/// A change made to a copy of a fixture image.
type Patch<'a> = Box<dyn Fn(&mut Image) + 'a>;

/// The v02 fixture (0 rounds, so fast to unlock), a patch applied to a copy
/// of it, and the copy opened and unlocked.
struct Probe {
    record: Value,
    image: Image,
}

impl Probe {
    fn new(prefix: &str) -> Self {
        let record = fixture(prefix);
        let image = Image::of(&record);
        Self { record, image }
    }

    /// The untouched fixture's tree and contents.
    fn baseline(&self) -> Tree {
        snapshot(&open_unlocked(self.image.bytes.clone(), &self.record).unwrap()).unwrap()
    }

    /// Open and unlock a copy that `patch` has changed.
    fn open(&self, patch: impl FnOnce(&mut Image)) -> Result<Vault, VaultError> {
        let mut image = self.image.clone();
        patch(&mut image);
        open_unlocked(image.bytes, &self.record)
    }

    /// The tree and contents of a patched copy.
    fn snapshot(&self, patch: impl FnOnce(&mut Image)) -> Result<Tree, Failure> {
        snapshot(&self.open(patch)?)
    }
}

#[test]
fn header_fields_without_effect_change_nothing() {
    // 01 §3.2, §3.3, §3.6, §3.8 and "Header fields without effect" in §13.2.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let garbage: Vec<u8> = (0..224u32).map(|i| (i * 37 + 11) as u8).collect();
    let patches: Vec<(&str, Patch<'_>)> = vec![
        (
            "garbage in legacy_area, reserved and the unused part of wrapped_key",
            Box::new(|image| {
                image.put(0x5C, &garbage[..20]);
                image.put(0xD8, &garbage[..40]);
                image.put(0x120, &garbage);
            }),
        ),
        ("cluster_size 0", Box::new(|image| image.put_i32(4, 0))),
        ("cluster_size 256", Box::new(|image| image.put_i32(4, 256))),
        ("cluster_size 777", Box::new(|image| image.put_i32(4, 777))),
        ("cluster_size -1", Box::new(|image| image.put_i32(4, -1))),
        (
            "failed_attempts 7",
            Box::new(|image| image.put_i32(0x90, 7)),
        ),
        (
            "negative kdf_rounds",
            Box::new(|image| image.put_i32(0x18, -5)),
        ),
        ("flags 2", Box::new(|image| image.put_i32(0x10, 2))),
        (
            "flags 0x7FFFFFFE",
            Box::new(|image| image.put_i32(0x10, 0x7FFF_FFFE)),
        ),
        (
            "all-zero file_guid",
            Box::new(|image| image.put(0x70, &[0; 16])),
        ),
        (
            "both timestamps zero",
            Box::new(|image| {
                image.put_i32(0x08, 0);
                image.put_i32(0xD4, 0);
            }),
        ),
        (
            "another type_guid",
            Box::new(|image| image.put(0x80, &garbage[..16])),
        ),
        (
            "token fields set",
            Box::new(|image| image.put(0x94, &garbage[..64])),
        ),
    ];
    for (name, patch) in patches {
        let tree = probe.snapshot(|image| patch(image));
        assert_eq!(tree.unwrap(), baseline, "{name}");
    }

    // What the header then reports.
    let vault = probe
        .open(|image| {
            image.put(0x70, &[0; 16]);
            image.put_i32(0x08, 0);
            image.put_i32(0xD4, 0);
        })
        .unwrap();
    assert!(vault.header().file_guid().is_nil());
    assert_eq!(vault.header().created_at().utc(), None);
    assert_eq!(vault.header().modified_at().to_iso8601(), None);
}

#[test]
fn garbage_in_the_header_slot_of_a_large_cluster_changes_nothing() {
    // 01 §2: with 4096-byte clusters the bytes from 512 to 4096 are unused.
    // v04 is AES-128: only 16 bytes of wrapped_key are meaningful (01 §5.4).
    let probe = Probe::new("v04");
    let tree = probe.snapshot(|image| {
        for (i, byte) in image.bytes[512..4096].iter_mut().enumerate() {
            *byte = (i * 31 + 7) as u8;
        }
        image.put(0x110, &[0xA5; 0xF0]);
    });
    assert_eq!(tree.unwrap(), probe.baseline());
}

#[test]
fn a_file_that_is_no_container_is_refused() {
    // 01 §3.2, §2 and "Layout" in §13.2.
    let probe = Probe::new("v02");
    let open = |patch: &dyn Fn(&mut Image)| probe.open(|image| patch(image)).unwrap_err();

    assert!(matches!(
        open(&|image| image.bytes[0] = 0x54),
        VaultError::NotAVault
    ));
    assert!(matches!(
        open(&|image| image.bytes[3] = 2),
        VaultError::UnsupportedVersion { version: 2 }
    ));
    assert!(matches!(
        open(&|image| image.bytes.truncate(511)),
        VaultError::NotAVault
    ));
    assert!(matches!(
        open(&|image| image.bytes.clear()),
        VaultError::NotAVault
    ));
    assert!(matches!(
        open(&|image| image.bytes.truncate(512)),
        VaultError::InvalidLayout {
            file_len: 512,
            cluster_size: 512
        }
    ));
    assert!(matches!(
        open(&|image| image.bytes.push(0)),
        VaultError::InvalidLayout { .. }
    ));
    assert!(matches!(
        open(&|image| {
            image.bytes.pop();
        }),
        VaultError::InvalidLayout { .. }
    ));
    // The same file read with another cluster size no longer fits.
    assert!(matches!(
        open(&|image| image.put_i32(4, 65536)),
        VaultError::InvalidLayout {
            cluster_size: 65536,
            ..
        }
    ));
}

#[test]
fn the_password_layer_rejects_a_changed_header() {
    // "Password layer" and "Cipher" in 01 §13.2.
    let probe = Probe::new("v02");
    let unlock_error = |patch: &dyn Fn(&mut Image)| probe.open(|image| patch(image)).unwrap_err();

    assert!(matches!(
        unlock_error(&|image| image.bytes[0x1C] ^= 1),
        VaultError::WrongPassword
    ));
    assert!(matches!(
        unlock_error(&|image| image.bytes[0x3C] ^= 1),
        VaultError::WrongPassword
    ));
    assert!(matches!(
        unlock_error(&|image| image.put_i32(0x18, 1)),
        VaultError::WrongPassword
    ));
    assert!(matches!(
        unlock_error(&|image| image.put_i32(0x10, 1)),
        VaultError::DeviceBound
    ));
    assert!(matches!(
        unlock_error(&|image| image.put_i32(0x14, 0)),
        VaultError::UnsupportedCipher { cipher_id: 0 }
    ));
    assert!(matches!(
        unlock_error(&|image| image.put_i32(0x14, 4)),
        VaultError::UnsupportedCipher { cipher_id: 4 }
    ));
    // A round count no vault has is refused without being run.
    assert!(matches!(
        unlock_error(&|image| image.put_i32(0x18, i32::MAX)),
        VaultError::KdfRoundsTooHigh {
            rounds: 0x7FFF_FFFF,
            limit: MAX_KDF_ROUNDS
        }
    ));

    // With an unsupported cipher the structure and the plain files are still
    // readable; encrypted files stay locked.
    let mut image = probe.image.clone();
    image.put_i32(0x14, 4);
    let mut vault = Vault::from_bytes(image.bytes).unwrap();
    assert!(vault.unlock(password(&probe.record)).is_err());
    assert_eq!(
        read(&vault, "b17.plain").unwrap(),
        read(
            &open_unlocked(probe.image.bytes.clone(), &probe.record).unwrap(),
            "b17.plain"
        )
        .unwrap()
    );
    assert!(matches!(read(&vault, "b17.enc"), Err(VaultError::Locked)));
}

#[test]
fn a_damaged_allocation_table_is_reported() {
    // 01 §6.2 and "Layout" in §13.2. v02 has two table pages.
    let probe = Probe::new("v02");
    assert_eq!(probe.image.table_pages(), [0, 128]);
    let first_entry = probe.image.table_offset(0);
    assert_eq!(first_entry, 0x200);

    for link in [[0x01, 0x81, 0, 0], [0, 0, 0, 0], [0x02, 0x80, 0, 0]] {
        assert!(matches!(
            probe.open(|image| image.put(first_entry, &link)),
            Err(VaultError::CorruptAllocationTable {
                cluster: 0,
                fault: TableFault::BadPageLink
            })
        ));
    }

    // A first page that claims to be the last one: the container opens, and
    // the first reference to a cluster of the second page fails.
    let vault = probe
        .open(|image| image.put(first_entry, &[0xFF, 0, 0, 0]))
        .unwrap();
    let failure = snapshot(&vault).unwrap_err();
    assert!(matches!(
        failure,
        Failure::Vault(VaultError::CorruptAllocationTable {
            fault: TableFault::NoEntry,
            cluster
        }) if cluster >= 128
    ));

    // The file ends before the second table page.
    assert!(matches!(
        probe.open(|image| image.bytes.truncate(129 * 512)),
        Err(VaultError::Truncated { cluster: 128 })
    ));
}

#[test]
fn directory_entries_are_read_like_banking4_reads_them() {
    // "Directory entries" in 01 §13.2.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let contents_of = |tree: &[(Node, Option<Vec<u8>>)], path: &str| {
        tree.iter()
            .find(|(node, _)| node.path == path)
            .unwrap_or_else(|| panic!("{path} is not listed"))
            .1
            .clone()
            .unwrap()
    };
    let without = |path: &str| -> Tree {
        baseline
            .iter()
            .filter(|(node, _)| node.path != path)
            .cloned()
            .collect()
    };
    let encrypted = probe.image.slot("b17.enc");
    let plain = probe.image.slot("many-clusters.plain");
    let plain_size = probe.image.size("many-clusters.plain");
    let empty = probe.image.slot("empty.enc");
    let dir = probe.image.slot("dir-a");
    let nested = probe.image.slot("dir-a/inner.enc");
    let beyond = probe.image.cluster_count() as i32 + 1000;

    // Slots Banking4 leaves unused: the entry is gone, nothing
    // else changes (01 §7.2).
    for (name, patch) in [
        (
            "name_len 55",
            Box::new(|image: &mut Image| image.bytes[encrypted + 9] = 55) as Patch<'_>,
        ),
        (
            "name_len 255",
            Box::new(|image| image.bytes[encrypted + 9] = 255),
        ),
        (
            "size -1",
            Box::new(|image| image.put_i32(encrypted + 4, -1)),
        ),
        (
            "first_cluster 0",
            Box::new(|image| image.put_i32(encrypted, 0)),
        ),
    ] {
        let tree = probe.snapshot(|image| patch(image)).unwrap();
        assert_eq!(tree, without("b17.enc"), "{name}");
    }

    // An empty name is listed (01 §7.2).
    let tree = probe
        .snapshot(|image| image.bytes[encrypted + 9] = 0)
        .unwrap();
    assert_eq!(tree.len(), baseline.len());
    assert_eq!(contents_of(&tree, ""), contents_of(&baseline, "b17.enc"));

    // Names are taken as they are: invalid UTF-8, an embedded zero byte, a
    // length shorter or longer than the text.
    for (name, patch, listed, stored) in [
        (
            "invalid UTF-8",
            Box::new(|image: &mut Image| image.bytes[encrypted + 11] = 0xFF) as Patch<'_>,
            "b\u{FFFD}7.enc",
            &b"b\xFF7.enc"[..],
        ),
        (
            "lone continuation byte and a cut two-byte character",
            Box::new(|image| image.put(encrypted + 10, &[0x80, 0xC3, 0x41])),
            "\u{FFFD}\u{FFFD}A.enc",
            &b"\x80\xC3A.enc"[..],
        ),
        (
            "embedded zero byte",
            Box::new(|image| image.bytes[encrypted + 11] = 0),
            "b\u{0}7.enc",
            &b"b\x007.enc"[..],
        ),
        (
            "name_len shorter than the text",
            Box::new(|image| image.bytes[encrypted + 9] = 3),
            "b17",
            &b"b17"[..],
        ),
        (
            "name_len longer than the text",
            Box::new(|image| image.bytes[encrypted + 9] = 12),
            "b17.enc\0\0\0\0\0",
            &b"b17.enc\0\0\0\0\0"[..],
        ),
    ] {
        let tree = probe.snapshot(|image| patch(image)).unwrap();
        let (node, contents) = tree
            .iter()
            .find(|(node, _)| node.path == listed)
            .unwrap_or_else(|| panic!("{name}: not listed"));
        assert_eq!(node.entry.name_bytes(), stored, "{name}");
        assert_eq!(*contents, Some(contents_of(&baseline, "b17.enc")), "{name}");
    }

    // Flag bits above 1 are ignored (01 §7.4).
    for flags in [0x05, 0x81] {
        let tree = probe
            .snapshot(|image| image.bytes[encrypted + 8] = flags)
            .unwrap();
        assert_eq!(
            contents_of(&tree, "b17.enc"),
            contents_of(&baseline, "b17.enc")
        );
    }
    let tree = probe
        .snapshot(|image| image.bytes[plain + 8] = 0x04)
        .unwrap();
    assert_eq!(
        contents_of(&tree, "many-clusters.plain"),
        contents_of(&baseline, "many-clusters.plain")
    );

    // A cleared encrypted flag returns the ciphertext as it is on disk.
    let tree = probe
        .snapshot(|image| image.bytes[encrypted + 8] = 0)
        .unwrap();
    let on_disk = probe
        .image
        .cluster_offset(probe.image.first_cluster("b17.enc"));
    assert_eq!(
        contents_of(&tree, "b17.enc"),
        &probe.image.bytes[on_disk..on_disk + 17]
    );
    assert_ne!(
        contents_of(&tree, "b17.enc"),
        contents_of(&baseline, "b17.enc")
    );

    // The size decides how much is read (01 §8.1).
    let tree = probe
        .snapshot(|image| image.put_i32(plain + 4, 700))
        .unwrap();
    assert_eq!(
        contents_of(&tree, "many-clusters.plain"),
        &contents_of(&baseline, "many-clusters.plain")[..700]
    );
    let tree = probe
        .snapshot(|image| image.put_i32(nested + 4, 20))
        .unwrap();
    assert_eq!(
        contents_of(&tree, "dir-a/inner.enc"),
        &contents_of(&baseline, "dir-a/inner.enc")[..20]
    );
    assert!(matches!(
        probe.snapshot(|image| image.put_i32(plain + 4, plain_size as i32 + 512)),
        Err(Failure::Vault(VaultError::CorruptChain {
            fault: ChainFault::EndsEarly,
            ..
        }))
    ));

    // A first cluster that is none (01 §10).
    assert!(matches!(
        probe.snapshot(|image| image.put_i32(plain, -1)),
        Err(Failure::Vault(VaultError::CorruptChain {
            cluster: -1,
            fault: ChainFault::EndsEarly
        }))
    ));
    for first in [-2, beyond, i32::MAX, i32::MIN] {
        assert!(matches!(
            probe.snapshot(|image| image.put_i32(plain, first)),
            Err(Failure::Vault(VaultError::CorruptChain {
                cluster,
                fault: ChainFault::OutOfRange
            })) if cluster == first
        ));
    }
    // An empty file never consults its chain (01 §8.1).
    for first in [-1, beyond] {
        let tree = probe.snapshot(|image| image.put_i32(empty, first)).unwrap();
        assert_eq!(tree.len(), baseline.len());
        assert_eq!(contents_of(&tree, "empty.enc"), b"");
    }

    // Directories (01 §7.4): without pages it is listed and empty; flags 3
    // and a non-zero size are still a directory with the same contents.
    let tree = probe.snapshot(|image| image.put_i32(dir, -1)).unwrap();
    let listed: Vec<&str> = tree.iter().map(|(node, _)| node.path.as_str()).collect();
    assert!(listed.contains(&"dir-a"));
    assert!(!listed.iter().any(|path| path.starts_with("dir-a/")));
    for patch in [
        Box::new(|image: &mut Image| image.bytes[dir + 8] = 3) as Patch<'_>,
        Box::new(|image| image.put_i32(dir + 4, 1234)),
    ] {
        let tree = probe.snapshot(|image| patch(image)).unwrap();
        let paths = |tree: &[(Node, Option<Vec<u8>>)]| -> Vec<String> {
            tree.iter().map(|(node, _)| node.path.clone()).collect()
        };
        assert_eq!(paths(&tree), paths(&baseline));
        assert_eq!(
            contents_of(&tree, "dir-a/dir-b/dir-c/deep.enc"),
            contents_of(&baseline, "dir-a/dir-b/dir-c/deep.enc")
        );
    }
}

#[test]
fn chains_are_followed_like_banking4_follows_them() {
    // "Chains" in 01 §13.2, on a plain file of several clusters.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let path = "many-clusters.plain";
    let chain = probe.image.chain(probe.image.first_cluster(path));
    assert!(chain.len() >= 3);
    let last = *chain.last().unwrap();
    let last_entry = probe.image.table_offset(last);
    let read_patched = |entry: [u8; 4]| {
        let vault = probe.open(|image| image.put(last_entry, &entry))?;
        read(&vault, path)
    };
    let expected = baseline
        .iter()
        .find(|(node, _)| node.path == path)
        .unwrap()
        .1
        .clone()
        .unwrap();

    // The successor is looked up for the last cluster too (01 §8.1).
    assert!(matches!(
        read_patched([0, 0, 0, 0]),
        Err(VaultError::CorruptChain {
            fault: ChainFault::Free,
            cluster
        }) if cluster == last as i32
    ));
    assert!(matches!(
        read_patched([2, 0, 0, 0]),
        Err(VaultError::CorruptAllocationTable {
            fault: TableFault::BadStatus,
            cluster
        }) if cluster == last as u32
    ));
    assert!(matches!(
        read_patched([1, 0, 0, 0]),
        Err(VaultError::CorruptChain {
            fault: ChainFault::OutOfRange,
            cluster: 0
        })
    ));
    assert!(matches!(
        read_patched([1, 0xFF, 0xFF, 0x00]),
        Err(VaultError::CorruptChain {
            fault: ChainFault::OutOfRange,
            cluster: 0xFFFF
        })
    ));

    // Harmless: garbage in the link bytes of an end entry, a chain that goes
    // on beyond what the size needs, and one that loops after the size is
    // satisfied.
    assert_eq!(read_patched([0xFF, 0x12, 0x34, 0x56]).unwrap(), expected);
    let [a, b, c, _] = (chain[1] as u32).to_le_bytes();
    assert_eq!(read_patched([1, a, b, c]).unwrap(), expected);
    let [a, b, c, _] = (chain[0] as u32).to_le_bytes();
    assert_eq!(read_patched([1, a, b, c]).unwrap(), expected);

    // Entries for clusters beyond the end of the file are ignored (01 §6.2).
    let per_page = 512 / 4;
    let count = probe.image.cluster_count();
    if count % per_page != 0 {
        let unused = probe.image.table_offset(count);
        let tree = probe.snapshot(|image| image.put(unused, &[1, 5, 0, 0]));
        assert_eq!(tree.unwrap(), baseline);
    }
}

#[test]
fn directory_loops_are_detected() {
    // 01 §6.3, §7.4.
    let probe = Probe::new("v02");
    let many = probe.image.chain(probe.image.first_cluster("many"));
    assert!(many.len() >= 2, "the directory has several pages");
    let last_entry = probe.image.table_offset(*many.last().unwrap());
    let [a, b, c, _] = (many[0] as u32).to_le_bytes();

    // A directory chain that leads back to its first page. Banking4 never
    // returns from this.
    let vault = probe
        .open(|image| image.put(last_entry, &[1, a, b, c]))
        .unwrap();
    let dir = lookup(&vault, "many").unwrap();
    assert!(matches!(
        vault.read_dir(&dir),
        Err(VaultError::CorruptDirectory {
            fault: ChainFault::Loop,
            ..
        })
    ));
    // The rest of the vault is unaffected.
    assert!(read(&vault, "dir-a/inner.enc").is_ok());

    // The same for the root directory fails when opening (01 §9 step 6).
    let root = probe.image.chain(probe.image.root_dir());
    let root_last = probe.image.table_offset(*root.last().unwrap());
    let [a, b, c, _] = (root[0] as u32).to_le_bytes();
    assert!(matches!(
        probe.open(|image| image.put(root_last, &[1, a, b, c])),
        Err(VaultError::CorruptDirectory {
            fault: ChainFault::Loop,
            ..
        })
    ));

    // A directory that contains one of its ancestors: two entries with the
    // same first cluster. The pages belong to the one that was listed first;
    // the other is refused, so a walk ends without keeping track itself.
    // Banking4 has no walk of its own (01 §13.4).
    let ancestor = probe.image.first_cluster("dir-a") as i32;
    let inner = probe.image.slot("dir-a/dir-b");
    let vault = probe.open(|image| image.put_i32(inner, ancestor)).unwrap();
    let dir_a = lookup(&vault, "dir-a").unwrap();
    let dir_b = lookup(&vault, "dir-a/dir-b").unwrap();
    assert_eq!(dir_b.first_cluster(), dir_a.first_cluster());
    assert_ne!(dir_a, dir_b);
    match vault.read_dir(&dir_b) {
        Err(VaultError::CrossLinked {
            cluster,
            first,
            second,
        }) => {
            assert_eq!(cluster as i32, ancestor);
            assert_eq!(first, "entry \"dir-a\"");
            assert_eq!(second, "entry \"dir-b\"");
        }
        other => panic!("{other:?}"),
    }
    // The ancestor itself lists as often as it is asked.
    assert_eq!(vault.read_dir(&dir_a).unwrap().len(), 4);
    assert!(matches!(
        walk(&vault),
        Err(Failure::Vault(VaultError::CrossLinked { .. }))
    ));

    // The same with the root directory as the ancestor.
    let root_first = probe.image.root_dir() as i32;
    let vault = probe
        .open(|image| image.put_i32(inner, root_first))
        .unwrap();
    let dir_b = lookup(&vault, "dir-a/dir-b").unwrap();
    match vault.read_dir(&dir_b) {
        Err(VaultError::CrossLinked { first, second, .. }) => {
            assert_eq!(first, "the root directory");
            assert_eq!(second, "entry \"dir-b\"");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_chain_that_loops_within_its_size_is_refused() {
    // 01 §6.3. Banking4 follows a file chain for as long as the size asks
    // and would hand out the same cluster again (not confirmed, 01 §13.4).
    // This reader refuses on purpose.
    let probe = Probe::new("v02");
    let path = "cluster-exact.plain";
    let first = probe.image.first_cluster(path);
    assert_eq!(probe.image.chain(first), [first], "a file of one cluster");
    let entry = probe.image.table_offset(first);
    let slot = probe.image.slot(path);
    let [a, b, c, _] = (first as u32).to_le_bytes();
    let read_with_size = |size: i32| {
        let vault = probe.open(|image| {
            image.put(entry, &[1, a, b, c]);
            image.put_i32(slot + 4, size);
        })?;
        read(&vault, path)
    };

    // As long as one cluster is enough, the link is beyond what the size
    // needs and is not followed (01 §8.1).
    let original = read(&probe.open(|_| ()).unwrap(), path).unwrap();
    assert_eq!(read_with_size(512).unwrap(), original);
    assert_eq!(read_with_size(1).unwrap(), original[..1]);
    // One byte more needs the cluster a second time.
    for size in [513, 1024, 1025, 64 * 1024, i32::MAX] {
        assert!(
            matches!(
                read_with_size(size),
                Err(VaultError::CorruptChain {
                    fault: ChainFault::Loop,
                    cluster
                }) if cluster == first as i32
            ),
            "size {size}"
        );
    }

    // A loop of several clusters, entered at its second cluster.
    let path = "many-clusters.plain";
    let chain = probe.image.chain(probe.image.first_cluster(path));
    let last_entry = probe.image.table_offset(*chain.last().unwrap());
    let slot = probe.image.slot(path);
    let [a, b, c, _] = (chain[1] as u32).to_le_bytes();
    let size = (chain.len() * 512 + 1) as i32;
    let vault = probe
        .open(|image| {
            image.put(last_entry, &[1, a, b, c]);
            image.put_i32(slot + 4, size);
        })
        .unwrap();
    assert!(matches!(
        read(&vault, path),
        Err(VaultError::CorruptChain {
            fault: ChainFault::Loop,
            cluster
        }) if cluster == chain[1] as i32
    ));
}

#[test]
fn entries_that_share_clusters_are_refused() {
    // 01 §6.3: a chain belongs to one file or directory. What a hostile
    // container would do: every account directory lists the same booking
    // file, so that its records are loaded once per account.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let contents = |path: &str| {
        let (_, contents) = baseline.iter().find(|(node, _)| node.path == path).unwrap();
        contents.clone().unwrap()
    };

    // Two files in different directories on one chain.
    let big = "many-clusters.plain";
    let chain = probe.image.chain(probe.image.first_cluster(big));
    let size = probe.image.size(big) as i32;
    let twin = probe.image.slot("dir-a/inner.plain");
    let vault = probe
        .open(|image| {
            image.put_i32(twin, chain[0] as i32);
            image.put_i32(twin + 4, size);
        })
        .unwrap();
    let first = lookup(&vault, big).unwrap();
    let second = lookup(&vault, "dir-a/inner.plain").unwrap();
    assert_eq!(vault.read_file(&first).unwrap(), contents(big));
    match vault.read_file(&second) {
        Err(VaultError::CrossLinked {
            cluster,
            first,
            second,
        }) => {
            assert_eq!(cluster as usize, chain[0]);
            assert_eq!(first, "entry \"many-clusters.plain\"");
            assert_eq!(second, "entry \"inner.plain\"");
        }
        other => panic!("{other:?}"),
    }
    // Reading the same entry again is not a clash: through the same value,
    // a copy of it, and the entry found by a second listing.
    assert_eq!(vault.read_file(&first).unwrap(), contents(big));
    assert_eq!(vault.read_file(&first.clone()).unwrap(), contents(big));
    assert_eq!(read(&vault, big).unwrap(), contents(big));
    // The refusal does not depend on which of the two comes first.
    let vault = probe
        .open(|image| {
            image.put_i32(twin, chain[0] as i32);
            image.put_i32(twin + 4, size);
        })
        .unwrap();
    assert_eq!(read(&vault, "dir-a/inner.plain").unwrap(), contents(big));
    assert!(matches!(
        read(&vault, big),
        Err(VaultError::CrossLinked { .. })
    ));

    // A chain that joins another one half way: the clash is found at the
    // first shared cluster, and nothing of the second file is returned.
    let middle = chain[chain.len() / 2];
    let short = "cluster-exact.plain";
    let short_first = probe.image.first_cluster(short);
    let short_slot = probe.image.slot(short);
    let [a, b, c, _] = (middle as u32).to_le_bytes();
    let vault = probe
        .open(|image| {
            let entry = image.table_offset(short_first);
            image.put(entry, &[1, a, b, c]);
            image.put_i32(short_slot + 4, 2 * 512);
        })
        .unwrap();
    assert_eq!(read(&vault, big).unwrap(), contents(big));
    assert!(matches!(
        read(&vault, short),
        Err(VaultError::CrossLinked { cluster, .. }) if cluster as usize == middle
    ));

    // Two directory entries with the same first cluster: every account
    // directory being the same directory.
    let many = probe.image.first_cluster("many") as i32;
    let other = probe.image.slot("Ordner-ü");
    let vault = probe.open(|image| image.put_i32(other, many)).unwrap();
    let listed = vault.read_dir(&lookup(&vault, "many").unwrap()).unwrap();
    assert!(listed.len() > 20);
    match vault.read_dir(&lookup(&vault, "Ordner-ü").unwrap()) {
        Err(VaultError::CrossLinked {
            cluster,
            first,
            second,
        }) => {
            assert_eq!(cluster as i32, many);
            assert_eq!(first, "entry \"many\"");
            assert_eq!(second, "entry \"Ordner-ü\"");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        vault.read_dir(&lookup(&vault, "many").unwrap()).unwrap(),
        listed
    );
    // The files of the directory that was listed all read.
    for entry in &listed {
        vault.read_file(entry).unwrap();
        vault.read_file(entry).unwrap();
    }

    // An untouched vault can be read any number of times, in any order.
    let vault = probe.open(|_| ()).unwrap();
    for _ in 0..3 {
        assert_eq!(snapshot(&vault).unwrap(), baseline);
    }
}

#[test]
fn a_failed_chain_fails_every_entry_that_leads_into_it() {
    // 01 §6.3: two directory entries on one chain that loops. Neither read
    // returns anything, so there is no claim to refuse the second one with;
    // it is told what the first one found, as often as it asks. (That this
    // costs one step and not another walk is checked where the steps can be
    // counted, in the unit tests of the vault module.)
    let probe = Probe::new("v02");
    let many = probe.image.chain(probe.image.first_cluster("many"));
    let last_entry = probe.image.table_offset(*many.last().unwrap());
    let [a, b, c, _] = (many[0] as u32).to_le_bytes();
    let other = probe.image.slot("Ordner-ü");
    let vault = probe
        .open(|image| {
            image.put(last_entry, &[1, a, b, c]);
            image.put_i32(other, many[0] as i32);
        })
        .unwrap();
    for _ in 0..3 {
        for path in ["many", "Ordner-ü", "Ordner-ü", "many"] {
            assert!(
                matches!(
                    vault.read_dir(&lookup(&vault, path).unwrap()),
                    Err(VaultError::CorruptDirectory {
                        fault: ChainFault::Loop,
                        cluster
                    }) if cluster == many[0] as i32
                ),
                "{path}"
            );
        }
    }
    // The rest of the vault is unaffected.
    assert!(read(&vault, "dir-a/inner.enc").is_ok());

    // A file that is cut short and a second entry on its chain that asks
    // for no more than is there: the failure of the first is no obstacle to
    // the second (01 §8.1), and nothing of the first was returned.
    let big = "many-clusters.plain";
    let chain = probe.image.chain(probe.image.first_cluster(big));
    let size = probe.image.size(big) as i32;
    let slot_of_big = probe.image.slot(big);
    let twin = probe.image.slot("dir-a/inner.plain");
    let vault = probe
        .open(|image| {
            image.put_i32(slot_of_big + 4, (chain.len() * 512 + 1) as i32);
            image.put_i32(twin, chain[0] as i32);
            image.put_i32(twin + 4, size);
        })
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            read(&vault, big),
            Err(VaultError::CorruptChain {
                fault: ChainFault::EndsEarly,
                cluster
            }) if cluster as usize == *chain.last().unwrap()
        ));
    }
    let original = read(&probe.open(|_| ()).unwrap(), big).unwrap();
    assert_eq!(read(&vault, "dir-a/inner.plain").unwrap(), original);
}

#[test]
fn allocation_table_pages_cannot_be_read_as_contents() {
    // 01 §6.2, §6.3: a container of 66,560 bytes, 129 clusters of 512 bytes,
    // so the allocation table has a second page in cluster 128. That page's
    // own entry says "end of chain" and its first four bytes are that entry.
    // A file entry that starts there would get `ff 00 00 00` as its
    // contents in Banking4. This reader refuses on purpose.
    let mut image = vec![0u8; 66_560];
    image[..4].copy_from_slice(&unhex("53 55 42 01"));
    image[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
    image[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
    // Table page 0: linked to page 1 in cluster 128; the root directory is
    // cluster 1.
    image[512..520].copy_from_slice(&unhex("01 80 00 00  ff 00 00 00"));
    image[129 * 512..129 * 512 + 4].copy_from_slice(&unhex("ff 00 00 00"));
    image[1024..1088].copy_from_slice(&slot(128, 4, 0, b"table.bin"));
    image[1088..1152].copy_from_slice(&slot(128, 0, 2, b"TableDir"));

    let vault = Vault::from_bytes(image.clone()).unwrap();
    assert_eq!(vault.cluster_count(), 129);
    let file = find_entry(vault.root(), "table.bin").unwrap();
    assert_eq!((file.first_cluster(), file.size()), (128, 4));
    let error = vault.read_file(file).unwrap_err();
    assert!(matches!(
        error,
        VaultError::CorruptChain {
            cluster: 128,
            fault: ChainFault::TablePage
        }
    ));
    assert_eq!(
        error.to_string(),
        "corrupt cluster chain: reference to a cluster of the allocation table (cluster 128)"
    );
    let dir = find_entry(vault.root(), "TableDir").unwrap();
    assert!(matches!(
        vault.read_dir(dir),
        Err(VaultError::CorruptDirectory {
            cluster: 128,
            fault: ChainFault::TablePage
        })
    ));

    // If page 0 is the last table page, cluster 128 is no table page and
    // has no table entry at all: the error of 01 §6.2 as before.
    image[512..516].copy_from_slice(&unhex("ff 00 00 00"));
    let vault = Vault::from_bytes(image).unwrap();
    let file = find_entry(vault.root(), "table.bin").unwrap();
    assert!(matches!(
        vault.read_file(file),
        Err(VaultError::CorruptAllocationTable {
            cluster: 128,
            fault: TableFault::NoEntry
        })
    ));
}

#[test]
fn damaged_ciphertext_is_not_detected() {
    // 01 §4: no integrity protection. One flipped bit garbles one block.
    let probe = Probe::new("v02");
    let path = "many-clusters.enc";
    let chain = probe.image.chain(probe.image.first_cluster(path));
    let flipped = probe.image.cluster_offset(chain[2]) + 37;
    let vault = probe.open(|image| image.bytes[flipped] ^= 0x10).unwrap();
    let damaged = read(&vault, path).unwrap();
    let intact = read(
        &open_unlocked(probe.image.bytes.clone(), &probe.record).unwrap(),
        path,
    )
    .unwrap();
    assert_eq!(damaged.len(), intact.len());
    let differing: Vec<usize> = (0..intact.len())
        .filter(|&i| damaged[i] != intact[i])
        .collect();
    assert!(!differing.is_empty());
    // All differences lie in the block that holds the flipped byte.
    let block = (2 * 512 + 37) / 16;
    assert!(differing.iter().all(|i| i / 16 == block));
}

#[test]
fn a_missing_tail_matters_only_if_it_was_in_use() {
    // "Truncation" in 01 §13.2. v02 ends with free clusters.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let tree = probe.snapshot(|image| {
        let len = image.bytes.len();
        image.bytes.truncate(len - 512);
    });
    assert_eq!(tree.unwrap(), baseline);

    let failure = probe
        .snapshot(|image| {
            let len = image.bytes.len();
            image.bytes.truncate(len - 8 * 512);
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        Failure::Vault(
            VaultError::CorruptChain {
                fault: ChainFault::OutOfRange,
                ..
            } | VaultError::CorruptDirectory {
                fault: ChainFault::OutOfRange,
                ..
            }
        )
    ));
}

// --- Confidentiality: nothing is read that the caller did not ask for --------------

/// The entries b4 must never read (the product's class of bank access data).
/// The container layer does not know these names; the tests below show that
/// it lets a caller honour them.
const NEVER_READ: [&str; 9] = [
    "Contacts",
    "EbicsContacts",
    "ContactsHTML",
    "SpoolerContacts",
    "OnlineContactKeys",
    "Outbox",
    "OutboxData",
    "ModelSettings",
    "OnlineTraces",
];

#[test]
fn an_entry_the_caller_skips_by_name_is_never_touched() {
    // Rename an entry of a vault in memory to each protected name and make
    // its contents unreadable: a first cluster outside the file and a size
    // above zero, so that any attempt to read it fails. Opening, unlocking,
    // listing and reading everything else must still succeed.
    let probe = Probe::new("v02");
    let baseline = probe.baseline();
    let victims = ["b17.enc", "many-clusters.plain", "dir-a/inner.enc"];
    for name in NEVER_READ {
        for victim in victims {
            let at = probe.image.slot(victim);
            let beyond = probe.image.cluster_count() as i32 + 7;
            let vault = probe
                .open(|image| {
                    let mut renamed = [0u8; 54];
                    renamed[..name.len()].copy_from_slice(name.as_bytes());
                    image.bytes[at + 9] = name.len() as u8;
                    image.put(at + 10, &renamed);
                    image.put_i32(at, beyond);
                    image.put_i32(at + 4, 4096);
                })
                .unwrap();

            let tree = walk(&vault).unwrap();
            assert_eq!(tree.len(), baseline.len(), "{name}");
            let mut skipped = 0;
            let mut read_back = 0;
            for node in &tree {
                if node.entry.name() == name {
                    skipped += 1;
                    continue;
                }
                if !node.entry.is_dir() {
                    let contents = vault.read_file(&node.entry).unwrap();
                    let (_, expected) = baseline
                        .iter()
                        .find(|(original, _)| original.path == node.path)
                        .unwrap();
                    assert_eq!(Some(contents), *expected, "{name}: {}", node.path);
                    read_back += 1;
                }
            }
            assert_eq!(skipped, 1, "{name}");
            let files = baseline.iter().filter(|(_, c)| c.is_some()).count();
            assert_eq!(read_back, files - 1, "{name}");

            // The trap works: reading the entry would have failed.
            let trapped = tree.iter().find(|node| node.entry.name() == name).unwrap();
            assert!(
                matches!(
                    vault.read_file(&trapped.entry),
                    Err(VaultError::CorruptChain { .. })
                ),
                "{name}"
            );
        }
    }
}

#[test]
fn a_protected_directory_can_be_skipped_without_listing_it() {
    // The same for a directory: its pages are only read by `read_dir`.
    let probe = Probe::new("v02");
    let at = probe.image.slot("many");
    let beyond = probe.image.cluster_count() as i32 + 7;
    let vault = probe
        .open(|image| {
            let mut renamed = [0u8; 54];
            renamed[..6].copy_from_slice(b"Outbox");
            image.bytes[at + 9] = 6;
            image.put(at + 10, &renamed);
            image.put_i32(at, beyond);
        })
        .unwrap();
    let outbox = find_entry(vault.root(), "Outbox").unwrap();
    assert!(outbox.is_dir());
    for entry in vault.root().iter().filter(|entry| entry.name() != "Outbox") {
        if entry.is_dir() {
            vault.read_dir(entry).unwrap();
        } else {
            vault.read_file(entry).unwrap();
        }
    }
    assert!(matches!(
        vault.read_dir(outbox),
        Err(VaultError::CorruptDirectory { .. })
    ));
}

// --- Fencing: an entry that is skipped keeps its storage to itself ------------------
//
// These use the demo vault of `tests/fixtures/vault` (password `demo`), which
// has all nine entries of bank access data, each with a marker as contents.

/// The demo vault and what a test needs to patch it.
fn demo() -> Image {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault/demo.sub");
    Image {
        bytes: std::fs::read(path).unwrap(),
        cluster_size: 512,
    }
}

fn open_demo(image: &Image) -> Vault {
    let mut vault = Vault::from_bytes(image.bytes.clone()).unwrap();
    vault.unlock("demo").unwrap();
    vault
}

/// What a caller does that reads by an allow-list: reserve every root entry
/// with a protected name, before reading anything. Returns the number of
/// clusters reserved.
fn reserve_never_read(vault: &Vault) -> usize {
    vault
        .root()
        .iter()
        .filter(|entry| NEVER_READ.contains(&entry.name()))
        .map(|entry| vault.reserve(entry))
        .sum()
}

/// Whether `result` is the clash of the entry `second` with the reserved
/// entry `first` at `cluster`.
fn clashes<T>(result: &Result<T, VaultError>, cluster: usize, first: &str, second: &str) -> bool {
    matches!(
        result,
        Err(VaultError::CrossLinked { cluster: at, first: a, second: b })
            if *at as usize == cluster
                && a == &format!("entry {first:?}")
                && b == &format!("entry {second:?}")
    )
}

const DECOY: &[u8] = b"B4-DECOY-MUST-NOT-BE-READ";

#[test]
fn an_allowed_name_on_the_clusters_of_a_skipped_entry_reads_nothing_once_it_is_reserved() {
    // The slot of `AcctGroups` gets the first cluster, the size and the flags
    // of `Contacts`: nine bytes, and the name stays.
    let mut image = demo();
    let (allowed, skipped) = (image.slot("AcctGroups"), image.slot("Contacts"));
    assert_eq!((allowed, skipped), (1024, 264_320));
    image.bytes.copy_within(skipped..skipped + 9, allowed);
    let cluster = image.first_cluster("Contacts");
    assert_eq!(cluster, 528);

    // Skipping by name alone is not enough: the allowed name delivers the
    // contents of the skipped entry, decrypted.
    let vault = open_demo(&image);
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    let contents = vault.read_file(entry).unwrap();
    assert_eq!(contents.len(), 400);
    assert!(contents.starts_with(DECOY));
    assert!(!vault.is_reserved(cluster as u32));

    // Reserved first, the read is refused, whatever is tried, and the
    // skipped entry itself is not read either.
    let vault = open_demo(&image);
    // Seven files of nine clusters, and two directories of one page with
    // one file of one cluster each.
    assert_eq!(reserve_never_read(&vault), 13);
    assert!(vault.is_reserved(cluster as u32));
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    for _ in 0..2 {
        let read = vault.read_file(entry);
        assert!(
            clashes(&read, cluster, "Contacts", "AcctGroups"),
            "{read:?}"
        );
    }
    let contacts = find_entry(vault.root(), "Contacts").unwrap();
    assert!(matches!(
        vault.read_file(contacts),
        Err(VaultError::Reserved)
    ));
    // Everything else is as readable as before.
    let payees = find_entry(vault.root(), "Payees").unwrap();
    assert_eq!(vault.read_file(payees).unwrap().len(), image.size("Payees"));
}

#[test]
fn reserving_needs_no_password_and_fences_what_is_below_a_directory() {
    let image = demo();
    let vault = Vault::from_bytes(image.bytes.clone()).unwrap();
    assert!(!vault.is_unlocked());
    let spooler = find_entry(vault.root(), "SpoolerContacts").unwrap();
    let page = image.first_cluster("SpoolerContacts");
    let file = image.first_cluster("SpoolerContacts/Contact1");
    // Its page and the cluster of the file in it.
    assert_eq!(vault.reserve(spooler), 2);
    assert!(vault.is_reserved(page as u32) && vault.is_reserved(file as u32));
    assert_eq!(vault.reserve(spooler), 0);
    assert!(matches!(vault.read_dir(spooler), Err(VaultError::Reserved)));

    // Mirror case (b): a directory with an allowed name and the pages of a
    // skipped one is not listed, and so nothing in it can be read.
    for skipped in ["SpoolerContacts", "OutboxData"] {
        let mut image = demo();
        let (to, from) = (image.slot("Acct757073160"), image.slot(skipped));
        image.bytes.copy_within(from..from + 4, to);
        let page = image.first_cluster(skipped);

        let vault = open_demo(&image);
        let entry = find_entry(vault.root(), "Acct757073160").unwrap();
        let listed = vault.read_dir(entry).unwrap();
        assert_eq!(listed.len(), 1, "without the fence it lists");
        assert!(vault.read_file(&listed[0]).unwrap().starts_with(DECOY));

        let vault = open_demo(&image);
        reserve_never_read(&vault);
        let entry = find_entry(vault.root(), "Acct757073160").unwrap();
        let listed = vault.read_dir(entry);
        assert!(
            clashes(&listed, page, skipped, "Acct757073160"),
            "{listed:?}"
        );
    }
}

#[test]
fn a_file_below_the_root_on_the_clusters_of_a_skipped_entry_is_refused() {
    // Mirror case (a): `Stmt` of an account directory points at `Contacts`.
    let mut image = demo();
    let (to, from) = (image.slot("Acct757073100/Stmt"), image.slot("Contacts"));
    image.bytes.copy_within(from..from + 9, to);
    let vault = open_demo(&image);
    reserve_never_read(&vault);
    let directory = find_entry(vault.root(), "Acct757073100").unwrap();
    let listed = vault.read_dir(directory).unwrap();
    let stmt = find_entry(&listed, "Stmt").unwrap();
    let read = vault.read_file(stmt);
    assert!(clashes(&read, 528, "Contacts", "Stmt"), "{read:?}");
    // Its neighbours in the directory read.
    let payments = find_entry(&listed, "Paymts").unwrap();
    assert_eq!(vault.read_file(payments).unwrap().len(), 633);
}

#[test]
fn a_chain_that_is_linked_or_sized_into_a_skipped_entry_is_refused_as_a_whole() {
    // Mirror case (c): `AcctGroups` keeps its own first cluster, and the
    // allocation table leads on from it into `Contacts`.
    let mut image = demo();
    let own = image.first_cluster("AcctGroups");
    let contacts = image.first_cluster("Contacts");
    assert_eq!(image.chain(own), [own]);
    let at = image.table_offset(own);
    let [a, b, c, _] = (contacts as u32).to_le_bytes();
    image.put(at, &[0x01, a, b, c]);

    // The link is beyond what the size needs: the file reads as it did.
    let intact = {
        let vault = open_demo(&demo());
        vault
            .read_file(find_entry(vault.root(), "AcctGroups").unwrap())
            .unwrap()
    };
    let vault = open_demo(&image);
    reserve_never_read(&vault);
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    assert_eq!(vault.read_file(entry).unwrap(), intact);

    // With a size that reaches across the link, nothing of it is returned,
    // its own first cluster included. Without the fence it would deliver
    // its own cluster followed by the contents of `Contacts`.
    let slot = image.slot("AcctGroups");
    image.put_i32(slot + 4, 512 + 400);
    let vault = open_demo(&image);
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    let unfenced = vault.read_file(entry).unwrap();
    assert!(unfenced[512..].starts_with(DECOY));
    let vault = open_demo(&image);
    reserve_never_read(&vault);
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    let read = vault.read_file(entry);
    assert!(
        clashes(&read, contacts, "Contacts", "AcctGroups"),
        "{read:?}"
    );

    // Mirror case (d): a size that runs past the file's own clusters into
    // the *second* cluster of a skipped file, which no directory slot names.
    // The whole chain of a reserved entry is fenced, not its first cluster.
    let mut image = demo();
    let keys = image.chain(image.first_cluster("OnlineContactKeys"));
    assert_eq!(keys.len(), 2);
    let own = image.first_cluster("Mandates");
    let at = image.table_offset(own);
    let [a, b, c, _] = (keys[1] as u32).to_le_bytes();
    image.put(at, &[0x01, a, b, c]);
    let slot = image.slot("Mandates");
    image.put_i32(slot + 4, 512 + 13);
    let vault = open_demo(&image);
    reserve_never_read(&vault);
    let entry = find_entry(vault.root(), "Mandates").unwrap();
    let read = vault.read_file(entry);
    assert!(
        clashes(&read, keys[1], "OnlineContactKeys", "Mandates"),
        "{read:?}"
    );

    // And a skipped entry whose own size says it is empty is fenced with its
    // chain all the same.
    let mut image = demo();
    let (allowed, skipped) = (image.slot("AcctGroups"), image.slot("Contacts"));
    image.bytes.copy_within(skipped..skipped + 9, allowed);
    image.put_i32(skipped + 4, 0);
    let vault = open_demo(&image);
    reserve_never_read(&vault);
    let entry = find_entry(vault.root(), "AcctGroups").unwrap();
    let read = vault.read_file(entry);
    assert!(clashes(&read, 528, "Contacts", "AcctGroups"), "{read:?}");
}

#[test]
fn a_skipped_entry_that_is_damaged_is_reserved_as_far_as_it_goes() {
    // Every protected entry in turn starts outside the file: reserving
    // reserves nothing for it, fails nothing, and the rest of the vault
    // reads.
    let intact = demo();
    for name in NEVER_READ {
        let mut image = intact.clone();
        let at = image.slot(name);
        image.put_i32(at, 0x00F0_0000);
        let vault = open_demo(&image);
        let entry = find_entry(vault.root(), name).unwrap();
        assert_eq!(vault.reserve(entry), 0, "{name}");
        assert!(reserve_never_read(&vault) > 0, "{name}");
        for entry in vault.root() {
            if NEVER_READ.contains(&entry.name()) {
                continue;
            }
            if entry.is_dir() {
                for inner in vault.read_dir(entry).unwrap() {
                    vault.read_file(&inner).unwrap();
                }
            } else {
                vault.read_file(entry).unwrap();
            }
        }
    }
}

// --- Robustness -------------------------------------------------------------------

/// Do everything a caller can do with an image and ignore every error: open,
/// unlock, list every directory, read every file. Returns how many entries
/// were seen. Must come back for any input.
fn exercise(bytes: Vec<u8>, record: &Value) -> usize {
    let image_len = bytes.len();
    let Ok(mut vault) = Vault::from_bytes(bytes) else {
        return 0;
    };
    let _ = unlock(&mut vault, record);

    // A damaged directory entry can point anywhere, also back at an ancestor.
    // No record of visited directories is kept here: the vault refuses pages
    // that belong to another entry, which is what ends this loop.
    let mut pending = vec![vault.root().to_vec()];
    let mut seen = 0;
    while let Some(entries) = pending.pop() {
        for entry in entries {
            seen += 1;
            assert!(entry.name_bytes().len() <= 54);
            if entry.is_dir() {
                assert!(matches!(vault.read_file(&entry), Err(VaultError::NotAFile)));
                if let Ok(children) = vault.read_dir(&entry) {
                    // A directory cannot list more than its pages hold.
                    assert!(children.len() <= image_len / 64);
                    pending.push(children);
                }
            } else {
                assert!(matches!(
                    vault.read_dir(&entry),
                    Err(VaultError::NotADirectory)
                ));
                if let Ok(contents) = vault.read_file(&entry) {
                    // Exactly the size the entry states, and never more than
                    // the container holds.
                    assert_eq!(contents.len(), entry.size() as usize);
                    assert!(contents.len() <= image_len);
                }
            }
        }
    }
    seen
}

/// The fixtures the robustness tests run on: three cluster sizes, all three
/// ciphers, one and two table pages, and few key-stretching rounds.
const ROBUSTNESS_FIXTURES: [&str; 3] = ["v02", "v04", "v05"];

#[test]
fn every_truncation_at_a_cluster_boundary_is_survived() {
    for prefix in ROBUSTNESS_FIXTURES {
        let record = fixture(prefix);
        let image = Image::of(&record);
        let total = record["entries"].as_array().unwrap().len();
        let clusters = image.bytes.len() / image.cluster_size;
        assert_eq!(exercise(image.bytes.clone(), &record), total, "{prefix}");

        // What must survive for the container to open at all (01 §9, steps 5
        // and 6): every table page and every page of the root directory.
        let last_table_page = *image.table_pages().last().unwrap();
        let last_root_page = *image.chain(image.root_dir()).iter().max().unwrap();
        let opens_from = last_table_page.max(last_root_page) + 2;
        // 01 §10: if nothing a reader needs was lost, everything reads;
        // otherwise some chain leads into the lost part.
        let intact_from = image.highest_needed_cluster() + 2;
        assert!(opens_from <= intact_from && intact_from <= clusters);
        let baseline = snapshot(&open_unlocked(image.bytes.clone(), &record).unwrap()).unwrap();

        for keep in 0..=clusters {
            let cut = image.bytes[..keep * image.cluster_size].to_vec();
            let seen = exercise(cut.clone(), &record);
            assert!(seen <= total, "{prefix}: {keep} clusters");
            assert_eq!(seen > 0, keep >= opens_from, "{prefix}: {keep} clusters");

            let strict = open_unlocked(cut, &record)
                .map_err(Failure::Vault)
                .and_then(|vault| snapshot(&vault));
            if keep >= intact_from {
                assert_eq!(strict.unwrap(), baseline, "{prefix}: {keep} clusters");
            } else {
                assert!(strict.is_err(), "{prefix}: {keep} clusters");
            }
        }

        // Cuts inside a cluster are refused outright (01 §2).
        for keep in 2..clusters {
            for odd in [1, image.cluster_size / 2, image.cluster_size - 1] {
                let cut = image.bytes[..keep * image.cluster_size - odd].to_vec();
                // With clusters larger than 512 bytes a cut inside the header
                // slot of the first cluster cannot happen here (keep >= 2).
                assert!(
                    matches!(
                        Vault::from_bytes(cut),
                        Err(VaultError::InvalidLayout { .. })
                    ),
                    "{prefix}: {keep} clusters minus {odd} bytes"
                );
            }
        }
    }
}

#[test]
fn single_byte_corruptions_are_survived() {
    // Damage one byte of the header, of an allocation table page or of a
    // directory page: the structures a reader trusts. Nothing may panic, hang
    // or produce more than the container holds.
    const PER_REGION: usize = 150;
    for prefix in ROBUSTNESS_FIXTURES {
        let record = fixture(prefix);
        let image = Image::of(&record);
        let total = record["entries"].as_array().unwrap().len();

        let header: Vec<Range<usize>> = std::iter::once(0..HEADER_SIZE).collect();
        let table: Vec<Range<usize>> = image
            .table_pages()
            .into_iter()
            .map(|page| image.cluster_range(page))
            .collect();
        let directories: Vec<Range<usize>> = image
            .directory_pages()
            .into_iter()
            .map(|page| image.cluster_range(page))
            .collect();
        assert!(directories.len() >= 5, "{prefix}");

        let mut rng = Rng(0x0B4D_5EED ^ u64::from(image.cluster_size as u32));
        let mut unchanged = 0;
        let mut cases = 0;
        for regions in [&header, &table, &directories] {
            for _ in 0..PER_REGION {
                let region = &regions[rng.below(regions.len())];
                let offset = region.start + rng.below(region.len());
                let flip = 1 + rng.below(255) as u8;
                let mut damaged = image.bytes.clone();
                damaged[offset] ^= flip;
                // One byte can remove entries or whole subtrees, turn an
                // unused slot into an entry, or make a file's clusters be
                // read as directory pages; `exercise` checks what comes back.
                if exercise(damaged, &record) == total {
                    unchanged += 1;
                }
                cases += 1;
            }
        }
        assert_eq!(cases, 3 * PER_REGION);
        // Many bytes of these pages are unused, so a good part of the cases
        // must leave the tree as it was: the test would be worthless if every
        // corruption simply failed to open.
        assert!(unchanged > cases / 10, "{prefix}: {unchanged}");
        assert!(unchanged < cases, "{prefix}");
    }
}

#[test]
fn every_header_byte_can_be_damaged() {
    // Each of the 512 header bytes, with all bits and with one bit flipped.
    let record = fixture("v02");
    let image = Image::of(&record);
    for offset in 0..HEADER_SIZE {
        for flip in [0xFF, 0x01, 0x80] {
            let mut damaged = image.bytes.clone();
            damaged[offset] ^= flip;
            exercise(damaged, &record);
        }
    }
}

#[test]
fn hostile_sizes_and_links_do_not_cause_large_allocations() {
    // An entry that claims 2 GiB and a table full of links, in a 130 KB file:
    // both fail from the structure alone.
    let probe = Probe::new("v02");
    let at = probe.image.slot("many-clusters.plain");
    let vault = probe.open(|image| image.put_i32(at + 4, i32::MAX)).unwrap();
    assert!(matches!(
        read(&vault, "many-clusters.plain"),
        Err(VaultError::CorruptChain {
            fault: ChainFault::EndsEarly,
            ..
        })
    ));

    // Every table entry of the first page links to cluster 1, which links to
    // itself: every chain loops.
    let record = &probe.record;
    let mut image = probe.image.clone();
    for cluster in 1..128 {
        let entry = image.table_offset(cluster);
        image.put(entry, &[1, 1, 0, 0]);
    }
    assert!(matches!(
        Vault::from_bytes(image.bytes.clone()),
        Err(VaultError::CorruptDirectory {
            fault: ChainFault::Loop,
            ..
        })
    ));
    assert_eq!(exercise(image.bytes, record), 0);

    // The same with a 2 GiB entry on a chain that loops: the read stops once
    // the chain is longer than the container (01 §6.3).
    let chain = probe
        .image
        .chain(probe.image.first_cluster("many-clusters.plain"));
    let last_entry = probe.image.table_offset(*chain.last().unwrap());
    let [a, b, c, _] = (chain[0] as u32).to_le_bytes();
    let vault = probe
        .open(|image| {
            image.put(last_entry, &[1, a, b, c]);
            image.put_i32(at + 4, i32::MAX);
        })
        .unwrap();
    assert!(matches!(
        read(&vault, "many-clusters.plain"),
        Err(VaultError::CorruptChain {
            fault: ChainFault::Loop,
            ..
        })
    ));
}

// --- Files on disk -------------------------------------------------------------------

#[test]
fn header_read_needs_only_the_header() {
    let record = fixture("v04");
    let bytes = fixture_bytes(&record);
    let dir = tempfile::tempdir().unwrap();

    // The first 512 bytes alone are enough for the header, and are not a
    // vault that can be opened.
    let header_only = dir.path().join("header-only.sub");
    std::fs::write(&header_only, &bytes[..HEADER_SIZE]).unwrap();
    assert_eq!(
        Header::read(&header_only).unwrap(),
        Header::read(fixture_path(&record)).unwrap()
    );
    assert!(matches!(
        Vault::open(&header_only),
        Err(VaultError::InvalidLayout { .. })
    ));

    let short = dir.path().join("short.sub");
    std::fs::write(&short, &bytes[..HEADER_SIZE - 1]).unwrap();
    assert!(matches!(Header::read(&short), Err(VaultError::NotAVault)));
    assert!(matches!(Vault::open(&short), Err(VaultError::NotAVault)));

    let empty = dir.path().join("empty.sub");
    std::fs::write(&empty, b"").unwrap();
    assert!(matches!(Header::read(&empty), Err(VaultError::NotAVault)));
    assert!(matches!(Vault::open(&empty), Err(VaultError::NotAVault)));

    let other = dir.path().join("other.sub");
    std::fs::write(&other, vec![b'x'; 4096]).unwrap();
    assert!(matches!(Header::read(&other), Err(VaultError::NotAVault)));
    assert!(matches!(Vault::open(&other), Err(VaultError::NotAVault)));
}

#[test]
fn a_missing_or_unreadable_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sub");
    for error in [
        Header::read(&missing).unwrap_err(),
        Vault::open(&missing).unwrap_err(),
    ] {
        let text = error.to_string();
        match error {
            VaultError::Io { path, source } => {
                assert_eq!(path, missing);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
                assert!(text.starts_with("cannot read vault file "));
            }
            other => panic!("unexpected error: {other}"),
        }
    }
    // A directory is not a vault file.
    assert!(matches!(
        Vault::open(dir.path()),
        Err(VaultError::Io { .. })
    ));
    assert!(matches!(
        Header::read(dir.path()),
        Err(VaultError::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_vault_is_opened_without_write_access_and_left_alone() {
    use std::os::unix::fs::PermissionsExt;

    // A read-only file in a read-only directory: opening for writing,
    // creating a lock file or writing anything back would fail or show.
    let record = fixture("v04");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sub");
    std::fs::copy(fixture_path(&record), &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

    Header::read(&path).unwrap();
    let mut vault = Vault::open(&path).unwrap();
    assert!(matches!(
        vault.unlock("not the password"),
        Err(VaultError::WrongPassword)
    ));
    vault.unlock(password(&record)).unwrap();
    let tree = snapshot(&vault).unwrap();
    assert_eq!(tree.len(), record["entries"].as_array().unwrap().len());
    drop(vault);

    let listing: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(listing, ["vault.sub"]);
    assert_eq!(
        sha256(&std::fs::read(&path).unwrap()),
        record["file_sha256"].as_str().unwrap()
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );

    // Let the temporary directory be removed again.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

// --- A real vault (local only) -----------------------------------------------------

/// Compare a real vault with a reference export of it.
///
/// Skipped unless `B4_TEST_VAULT` (the vault file) and
/// `B4_TEST_PASSWORD_FILE` (a file whose first line is the password; it may
/// be `/dev/stdin`) are set. `B4_TEST_RAW_DIR` names a reference export: a
/// directory that holds the decrypted files under the same paths; files
/// without a counterpart there are only read. `B4_TEST_EXPECT_IDENTICAL` is
/// the number of files that must compare equal.
///
/// Entries with a protected name are skipped before any of their contents is
/// read. The output is counts and timings only; a mismatch is reported by
/// path.
#[test]
fn a_real_vault_matches_its_reference_export() {
    let (Some(vault_path), Some(password_file)) = (
        std::env::var_os("B4_TEST_VAULT"),
        std::env::var_os("B4_TEST_PASSWORD_FILE"),
    ) else {
        eprintln!("skipped: B4_TEST_VAULT and B4_TEST_PASSWORD_FILE are not set");
        return;
    };
    let raw_dir = std::env::var_os("B4_TEST_RAW_DIR").map(PathBuf::from);
    let password = zeroize::Zeroizing::new(
        std::fs::read_to_string(password_file).expect("the password file is readable"),
    );
    let password = password.lines().next().unwrap_or("");

    let started = Instant::now();
    let mut vault = Vault::open(&vault_path).expect("the vault opens");
    let opened = started.elapsed();
    vault.unlock(password).expect("the vault unlocks");
    let unlocked = started.elapsed();

    // Walk by name first; read only what is not protected.
    let mut visited = HashSet::from([vault.header().root_dir_cluster()]);
    let mut pending = vec![(String::new(), vault.root().to_vec())];
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut directories = 0;
    while let Some((prefix, entries)) = pending.pop() {
        for entry in entries {
            if NEVER_READ.contains(&entry.name()) {
                skipped += 1;
                continue;
            }
            let path = format!("{prefix}{}", entry.name());
            if entry.is_dir() {
                directories += 1;
                assert!(visited.insert(entry.first_cluster()), "directory cycle");
                let children = vault.read_dir(&entry).expect("the directory lists");
                pending.push((format!("{path}/"), children));
            } else {
                let contents = vault.read_file(&entry).expect("the file reads");
                files.push((path, contents));
            }
        }
    }
    let read_all = started.elapsed();

    let mut identical = 0;
    let mut different = Vec::new();
    let mut without_reference = 0;
    if let Some(raw_dir) = &raw_dir {
        for (path, contents) in &files {
            match std::fs::read(raw_dir.join(path)) {
                Ok(reference) if reference == *contents => identical += 1,
                Ok(_) => different.push(path.as_str()),
                Err(_) => without_reference += 1,
            }
        }
    }
    let bytes: usize = files.iter().map(|(_, contents)| contents.len()).sum();
    println!(
        "real vault: {} bytes, cluster size {}, {} rounds, {} directories, {} files read \
         ({bytes} bytes), {skipped} entries skipped by name",
        vault.file_len(),
        vault.header().cluster_size(),
        vault.header().kdf_rounds(),
        directories,
        files.len(),
    );
    println!(
        "compared: {identical} identical, {} different, {without_reference} without reference",
        different.len()
    );
    println!(
        "timing: open {:.1} ms, unlock {:.1} ms, list and read all {:.1} ms, total {:.1} ms",
        opened.as_secs_f64() * 1e3,
        (unlocked - opened).as_secs_f64() * 1e3,
        (read_all - unlocked).as_secs_f64() * 1e3,
        read_all.as_secs_f64() * 1e3,
    );
    assert!(different.is_empty(), "files that differ: {different:?}");
    if let Some(expected) = std::env::var_os("B4_TEST_EXPECT_IDENTICAL") {
        let expected: usize = expected.to_str().unwrap().parse().unwrap();
        assert_eq!(identical, expected);
    }
}
