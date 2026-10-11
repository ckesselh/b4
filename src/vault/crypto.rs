//! From the password to the content key (01 §5), and the decryption of file
//! contents (01 §8.2).
//!
//! Every use of AES in the container is ECB without padding (01 §4): each
//! 16-byte block stands on its own.
//!
//! Secrets held here (the UTF-16 form of the password, the state of the hash
//! it runs through, the pass hash, the pass key, the content key) are wiped
//! when they go out of scope. The key schedules the `aes` crate builds from
//! them live on the stack for the length of one call; whether those are wiped
//! too depends on that crate's `zeroize` feature.

use std::fmt;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::inout::InOutBuf;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128Dec, Aes192Dec, Aes256Dec, Aes256Enc};
use zeroize::{Zeroize, Zeroizing};

use super::error::VaultError;
use super::header::{Cipher, Guid, Header};
use super::ripemd160::Ripemd160;

/// The constant that starts every pass hash (01 §5.1).
const PASS_CONSTANT: [u8; 32] = [
    0x90, 0x3c, 0x4a, 0x16, 0x40, 0xb4, 0x43, 0x81, 0xb1, 0x99, 0x86, 0x6a, 0xa4, 0x51, 0x6d, 0x19,
    0x0c, 0xb0, 0xdc, 0xb8, 0xef, 0xa1, 0x4c, 0x1a, 0x97, 0xce, 0x04, 0x3b, 0x34, 0x1a, 0x34, 0xcf,
];

/// The largest `kdf_rounds` value [`Vault::unlock`](super::Vault::unlock)
/// accepts.
///
/// The format itself has no limit (01 §5.2): Banking4 runs whatever the
/// header says, up to 2^31 rounds. It writes 100,000. A single damaged header
/// byte can turn that into a number that keeps a reader busy for minutes, so
/// this reader refuses anything above one hundred times the value Banking4
/// writes with [`VaultError::KdfRoundsTooHigh`] instead of appearing to hang.
pub const MAX_KDF_ROUNDS: u32 = 10_000_000;

/// The content key of an unlocked vault (01 §5.4): one key for every encrypted
/// file of the container. Wiped on drop.
pub(crate) enum ContentKey {
    Aes128(Zeroizing<[u8; 16]>),
    Aes192(Zeroizing<[u8; 24]>),
    Aes256(Zeroizing<[u8; 32]>),
}

impl ContentKey {
    /// Decrypt `buf` in place, block by block (01 §8.2). `buf` must be a
    /// whole number of 16-byte blocks; a shorter remainder would be left as
    /// it is.
    pub(crate) fn decrypt(&self, buf: &mut [u8]) {
        // The schedule is rebuilt per call (a few hundred nanoseconds) so that
        // the only long-lived copy of the key is the wiped one in `self`.
        match self {
            Self::Aes128(key) => decrypt_blocks(&Aes128Dec::new(as_key(key)), buf),
            Self::Aes192(key) => decrypt_blocks(&Aes192Dec::new(as_key(key)), buf),
            Self::Aes256(key) => decrypt_blocks(&Aes256Dec::new(as_key(key)), buf),
        }
    }

    /// The key bytes, for comparing with the fixture manifest.
    #[cfg(test)]
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Aes128(key) => &key[..],
            Self::Aes192(key) => &key[..],
            Self::Aes256(key) => &key[..],
        }
    }
}

/// Never shows the key.
impl fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cipher = match self {
            Self::Aes128(_) => Cipher::Aes128,
            Self::Aes192(_) => Cipher::Aes192,
            Self::Aes256(_) => Cipher::Aes256,
        };
        write!(f, "ContentKey({cipher}, <redacted>)")
    }
}

/// Run phase 2 of opening a container (01 §9, steps 7 to 11): check the
/// password against the header and recover the content key.
///
/// `device_guid` is consulted only if the header says the vault is bound to a
/// device; for any other vault it makes no difference (01 §3.6).
pub(crate) fn derive_content_key(
    header: &Header,
    password: &str,
    device_guid: Option<&Guid>,
) -> Result<ContentKey, VaultError> {
    // Step 7: without a known cipher there is no content key to recover.
    let cipher = header.cipher().ok_or(VaultError::UnsupportedCipher {
        cipher_id: header.cipher_id(),
    })?;
    // Step 8.
    let device_guid = if header.is_device_bound() {
        Some(device_guid.ok_or(VaultError::DeviceBound)?)
    } else {
        None
    };
    let rounds = header.effective_kdf_rounds();
    if rounds > MAX_KDF_ROUNDS {
        return Err(VaultError::KdfRoundsTooHigh {
            rounds,
            limit: MAX_KDF_ROUNDS,
        });
    }

    // Step 9.
    let mut pass_key = pass_hash(password, device_guid);
    stretch(&mut pass_key, header.salt(), rounds);

    // Step 10 (01 §5.3): the password layer is AES-256 whatever the content
    // cipher is.
    let unwrap = Aes256Dec::new(as_key(&pass_key));
    let mut check = *header.salt_check();
    decrypt_blocks(&unwrap, &mut check);
    if !constant_time_eq(&check, header.salt()) {
        return Err(VaultError::WrongPassword);
    }

    // Step 11 (01 §5.4): the key occupies the first one or two blocks of
    // `wrapped_key`. Decrypting two blocks covers every key length; blocks
    // are independent, so a surplus block changes nothing.
    let mut plain = Zeroizing::new([0u8; 32]);
    for (out, wrapped) in plain.iter_mut().zip(header.wrapped_key()) {
        *out = *wrapped;
    }
    decrypt_blocks(&unwrap, &mut plain[..]);
    Ok(match cipher {
        Cipher::Aes128 => ContentKey::Aes128(prefix(&plain)),
        Cipher::Aes192 => ContentKey::Aes192(prefix(&plain)),
        Cipher::Aes256 => ContentKey::Aes256(prefix(&plain)),
    })
}

/// The pass hash (01 §5.1): RIPEMD-160 over the constant, the UTF-16LE
/// password and, for a device-bound vault, the device GUID, expanded from 20
/// to 32 bytes with a running XOR.
fn pass_hash(password: &str, device_guid: Option<&Guid>) -> Zeroizing<[u8; 32]> {
    // UTF-16 never has more code units than UTF-8 has bytes, so the buffer
    // never grows: a reallocation would leave an unwiped copy behind.
    let mut utf16 = Zeroizing::new(Vec::with_capacity(password.len().saturating_mul(2)));
    for unit in password.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }

    // The hasher copies what it is given into its block buffer, so it is one
    // that wipes itself: the password must not outlive this function in it.
    let mut hasher = Ripemd160::new();
    hasher.update(&PASS_CONSTANT);
    hasher.update(&utf16[..]);
    if let Some(guid) = device_guid {
        hasher.update(guid.as_bytes());
    }
    let digest = hasher.finalize();

    // h[0] = d[0], h[i] = h[i-1] XOR d[i mod 20].
    let mut hash = Zeroizing::new([0u8; 32]);
    let mut running = 0u8;
    for (out, d) in hash.iter_mut().zip(digest.iter().cycle()) {
        running ^= *d;
        *out = running;
    }
    // The last byte of the hash.
    running.zeroize();
    hash
}

/// Key stretching (01 §5.2): `rounds` times, encrypt the salt with AES-256
/// under the current key and XOR the result into the key.
fn stretch(key: &mut [u8; 32], salt: &[u8; 32], rounds: u32) {
    let mut x = [0u8; 32];
    for _ in 0..rounds {
        // The key changes every round, so the schedule is rebuilt every
        // round. Only the encryption half is needed.
        let cipher = Aes256Enc::new(as_key(&*key));
        x = *salt;
        encrypt_blocks(&cipher, &mut x);
        for (k, x) in key.iter_mut().zip(&x) {
            *k ^= *x;
        }
    }
    x.zeroize();
}

/// View a fixed-size key as the array type the `aes` crate takes.
fn as_key<const N: usize, L>(key: &[u8; N]) -> &GenericArray<u8, L>
where
    L: aes::cipher::generic_array::ArrayLength<u8>,
    for<'a> &'a GenericArray<u8, L>: From<&'a [u8; N]>,
{
    key.into()
}

/// The first `N` bytes of the unwrapped key block.
fn prefix<const N: usize>(plain: &[u8; 32]) -> Zeroizing<[u8; N]> {
    let mut key = Zeroizing::new([0u8; N]);
    for (out, byte) in key.iter_mut().zip(plain) {
        *out = *byte;
    }
    key
}

/// Encrypt every whole 16-byte block of `buf` in place, each on its own.
fn encrypt_blocks<C: BlockEncrypt>(cipher: &C, buf: &mut [u8]) {
    let (blocks, _partial) = InOutBuf::from(buf).into_chunks::<C::BlockSize>();
    cipher.encrypt_blocks_inout(blocks);
}

/// Decrypt every whole 16-byte block of `buf` in place, each on its own. The
/// `aes` crate processes several blocks at once where the CPU supports it.
fn decrypt_blocks<C: BlockDecrypt>(cipher: &C, buf: &mut [u8]) {
    let (blocks, _partial) = InOutBuf::from(buf).into_chunks::<C::BlockSize>();
    cipher.decrypt_blocks_inout(blocks);
}

/// Compare without stopping at the first difference.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_digit(16).unwrap() as u8)
            .collect();
        digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect()
    }

    fn example_salt() -> [u8; 32] {
        std::array::from_fn(|i| 0x10 + i as u8)
    }

    fn example_device() -> Guid {
        "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap()
    }

    #[test]
    fn ripemd160_of_the_example_password() {
        // 01 §12.3, first two lines.
        let utf16 = unhex("62 00 34 00 2d 00 65 00 78 00 61 00 6d 00 70 00 6c 00 65 00");
        let encoded: Vec<u8> = "b4-example"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(encoded, utf16);
        let mut hasher = Ripemd160::new();
        hasher.update(&PASS_CONSTANT);
        hasher.update(&utf16);
        assert_eq!(
            hex(&hasher.finalize()[..]),
            "1403fb19a8697009e1f2a5f2914e8f33455ab73e"
        );
    }

    #[test]
    fn pass_hash_and_stretching_of_the_worked_example() {
        // 01 §12.3.
        let mut key = pass_hash("b4-example", None);
        assert_eq!(
            hex(&key[..]),
            "1417ecf55d34444dac5efb0998d6596a2f75c2fce8eb1009a1c8b8b150a207f5"
        );
        stretch(&mut key, &example_salt(), 1);
        assert_eq!(
            hex(&key[..]),
            "a430ad0916dcf3486233eb04b3cfd682d468ef6c67e378a9f9bf01eae499e481"
        );
        stretch(&mut key, &example_salt(), 1);
        assert_eq!(
            hex(&key[..]),
            "f9e49e025de6233ce6a3b6035eab5d41e968829cd7bc5796bb2fcfeb11969e4e"
        );

        // Two rounds at once are the same as one after the other, and no
        // rounds leave the key alone.
        let mut at_once = pass_hash("b4-example", None);
        stretch(&mut at_once, &example_salt(), 2);
        assert_eq!(at_once, key);
        stretch(&mut at_once, &example_salt(), 0);
        assert_eq!(at_once, key);
    }

    #[test]
    fn device_guid_enters_the_pass_hash() {
        // 01 §12.3, the comparison with security_flags = 1.
        let mut key = pass_hash("b4-example", Some(&example_device()));
        assert_eq!(
            hex(&key[..]),
            "824fed25295a080cbc759b6565c56e0db849fc34b67bd9111d6e3c388841af51"
        );
        stretch(&mut key, &example_salt(), 2);
        assert_eq!(
            hex(&key[..]),
            "5f7d4a8c34693819d877d4a1a59466999a060da6f2f08603f12f15ae772bd33d"
        );
    }

    #[test]
    fn empty_password_hash() {
        // 01 §5.5.
        assert_eq!(
            hex(&pass_hash("", None)[..]),
            "811e64d049cd235c8254ee2f1ee119670daa2eac2db2c87ce5618ff02ef84283"
        );
    }

    #[test]
    fn password_is_hashed_as_utf16_without_normalisation() {
        // 01 §5.1: surrogate pairs for characters outside the BMP, no
        // trimming, no case folding, no normalisation.
        let hashes = [
            pass_hash("pw", None),
            pass_hash("PW", None),
            pass_hash(" pw", None),
            pass_hash("pw ", None),
            pass_hash("p\u{e4}", None),
            pass_hash("pa\u{308}", None),
            pass_hash("p\u{1f600}", None),
        ];
        for (i, a) in hashes.iter().enumerate() {
            for b in hashes.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    /// The worked example's header (01 §12.2) with the given cipher id, flags
    /// and round count.
    fn example_header(cipher_id: i32, flags: u32, rounds: i32) -> Header {
        let mut h = vec![0u8; 512];
        h[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
        h[0x04..0x08].copy_from_slice(&512i32.to_le_bytes());
        h[0x0C..0x10].copy_from_slice(&1i32.to_le_bytes());
        h[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
        h[0x14..0x18].copy_from_slice(&cipher_id.to_le_bytes());
        h[0x18..0x1C].copy_from_slice(&rounds.to_le_bytes());
        h[0x1C..0x3C].copy_from_slice(&example_salt());
        h[0x3C..0x5C].copy_from_slice(&unhex(
            "65 73 56 35 7a a3 33 06 22 b6 cb 47 01 a8 cc 10
             a7 1d 92 cd be 77 3d 0e d1 a8 4d 52 e5 ed b5 cb",
        ));
        h[0x100..0x120].copy_from_slice(&unhex(
            "02 a5 79 19 71 48 02 c4 b9 b2 10 9b 65 a2 42 23
             17 5a b6 8f 4c aa e3 c1 e6 6f bb a1 cb e7 14 e1",
        ));
        Header::parse(&h).unwrap()
    }

    #[test]
    fn content_key_of_the_worked_example() {
        // 01 §12.3: the content key is a0 a1 .. bf.
        let expected: Vec<u8> = (0xa0..=0xbf).collect();
        let key = derive_content_key(&example_header(3, 0, 2), "b4-example", None).unwrap();
        assert_eq!(key.bytes(), expected);
        // Flags value 2 changes nothing (01 §3.6), and a device GUID is
        // ignored unless bit 0 is set.
        let key = derive_content_key(&example_header(3, 2, 2), "b4-example", None).unwrap();
        assert_eq!(key.bytes(), expected);
        let key = derive_content_key(
            &example_header(3, 0, 2),
            "b4-example",
            Some(&example_device()),
        )
        .unwrap();
        assert_eq!(key.bytes(), expected);
        // The same wrapped block read as a shorter key is its prefix (01 §5.4).
        let key = derive_content_key(&example_header(1, 0, 2), "b4-example", None).unwrap();
        assert_eq!(key.bytes(), &expected[..16]);
        let key = derive_content_key(&example_header(2, 0, 2), "b4-example", None).unwrap();
        assert_eq!(key.bytes(), &expected[..24]);
    }

    #[test]
    fn unlock_errors_come_in_the_order_of_the_opening_sequence() {
        // 01 §9: cipher (step 7) before device binding (step 8) before the
        // password check (step 10).
        assert!(matches!(
            derive_content_key(&example_header(0, 1, 2), "wrong", None),
            Err(VaultError::UnsupportedCipher { cipher_id: 0 })
        ));
        assert!(matches!(
            derive_content_key(&example_header(4, 0, 2), "b4-example", None),
            Err(VaultError::UnsupportedCipher { cipher_id: 4 })
        ));
        assert!(matches!(
            derive_content_key(&example_header(3, 1, 2), "b4-example", None),
            Err(VaultError::DeviceBound)
        ));
        // Bit 0 set on a vault that was not created that way: the GUID goes
        // into the hash and the check fails (01 §13.2, "Password layer").
        assert!(matches!(
            derive_content_key(
                &example_header(3, 1, 2),
                "b4-example",
                Some(&example_device())
            ),
            Err(VaultError::WrongPassword)
        ));
        for wrong in ["", "b4-Example", "b4-example ", " b4-example", "b4-exampl"] {
            assert!(matches!(
                derive_content_key(&example_header(3, 0, 2), wrong, None),
                Err(VaultError::WrongPassword)
            ));
        }
        // A changed round count is a wrong key.
        assert!(matches!(
            derive_content_key(&example_header(3, 0, 1), "b4-example", None),
            Err(VaultError::WrongPassword)
        ));
        // Negative rounds are no rounds (01 §5.2), not an error of their own.
        assert!(matches!(
            derive_content_key(&example_header(3, 0, -5), "b4-example", None),
            Err(VaultError::WrongPassword)
        ));
    }

    #[test]
    fn absurd_round_counts_are_refused_at_once() {
        // The error variant is the proof: running the rounds instead would
        // end in another result (and take minutes).
        let limit = i32::try_from(MAX_KDF_ROUNDS).unwrap();
        for rounds in [limit + 1, i32::MAX] {
            assert!(matches!(
                derive_content_key(&example_header(3, 0, rounds), "b4-example", None),
                Err(VaultError::KdfRoundsTooHigh { .. })
            ));
        }
    }

    #[test]
    fn aes_decrypts_the_fips_197_vectors() {
        // FIPS-197 appendix C: plaintext 00112233445566778899aabbccddeeff
        // under the keys 000102.. of 16, 24 and 32 bytes.
        let plain = unhex("00112233445566778899aabbccddeeff");
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        for (content_key, ciphertext) in [
            (
                ContentKey::Aes128(prefix(&key)),
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                ContentKey::Aes192(prefix(&key)),
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                ContentKey::Aes256(prefix(&key)),
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ] {
            // Three blocks and a partial one: ECB, every block on its own;
            // the partial block is not touched.
            let block = unhex(ciphertext);
            let mut buf = [&block[..], &block[..], &block[..], &[0xEE; 5][..]].concat();
            content_key.decrypt(&mut buf);
            assert_eq!(&buf[..16], plain);
            assert_eq!(&buf[16..32], plain);
            assert_eq!(&buf[32..48], plain);
            assert_eq!(&buf[48..], [0xEE; 5]);
        }
    }

    #[test]
    fn hello_txt_of_the_worked_example_decrypts() {
        // 01 §12.6.
        let key: [u8; 32] = std::array::from_fn(|i| 0xa0 + i as u8);
        let mut buf = unhex(
            "c4 42 97 01 b0 d5 57 67 3a 17 1f 83 08 7f 65 2c
             8d 1f 05 8b c2 0b ee 8e 8c 35 cc a2 bc 6a d7 47",
        );
        ContentKey::Aes256(prefix(&key)).decrypt(&mut buf);
        assert_eq!(&buf[..20], b"Hello, synthetic b4!");
        assert_eq!(&buf[20..], [0u8; 12]);
    }

    #[test]
    fn debug_output_does_not_show_the_key() {
        let key = ContentKey::Aes256(Zeroizing::new([0xAB; 32]));
        let text = format!("{key:?}");
        assert_eq!(text, "ContentKey(AES-256, <redacted>)");
    }

    #[test]
    fn content_keys_of_all_fixtures_match_the_manifest() {
        // Key recovery on its own (01 §5.4), for AES-128, AES-192 and
        // AES-256, with 0 to 100000 rounds, with and without a device GUID.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("MANIFEST.json")).unwrap()).unwrap();
        let vaults = manifest["vaults"].as_array().unwrap();
        assert_eq!(vaults.len(), 9);
        for vault in vaults {
            let file = vault["file"].as_str().unwrap();
            let header = Header::read(dir.join(file)).unwrap();
            let device: Option<Guid> = vault["device_guid"].as_str().map(|g| g.parse().unwrap());
            let key = derive_content_key(
                &header,
                vault["password"].as_str().unwrap(),
                device.as_ref(),
            )
            .unwrap();
            assert_eq!(
                hex(key.bytes()),
                vault["content_key"].as_str().unwrap(),
                "{file}"
            );
            assert_eq!(
                key.bytes().len(),
                header.cipher().unwrap().key_len(),
                "{file}"
            );
        }
    }
}
