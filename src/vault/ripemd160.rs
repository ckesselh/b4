//! RIPEMD-160, the hash of the pass hash (01 §5.1).
//!
//! The function is the one of Dobbertin, Bosselaers and Preneel, "RIPEMD-160:
//! a strengthened version of RIPEMD" (1996); the constants and tables below
//! are those of its appendix A.
//!
//! b4 has its own implementation for one reason: the password runs through
//! this hash, and a hasher from a general-purpose crate keeps the last block
//! of its input in a buffer that it does not wipe. Everything this one holds
//! (chaining state, block buffer, message schedule, length) is wiped when the
//! hasher is dropped, whether or not a digest was taken.

use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Length of a digest in bytes.
pub(crate) const DIGEST_LEN: usize = 20;

/// Length of a message block in bytes.
const BLOCK_LEN: usize = 64;

/// Where the message length starts in the last block.
const LENGTH_OFFSET: usize = BLOCK_LEN - 8;

/// The chaining state before the first block.
const INITIAL_STATE: [u32; 5] = [
    0x6745_2301,
    0xEFCD_AB89,
    0x98BA_DCFE,
    0x1032_5476,
    0xC3D2_E1F0,
];

/// Added constants of the left line, one per round.
const K_LEFT: [u32; 5] = [
    0x0000_0000,
    0x5A82_7999,
    0x6ED9_EBA1,
    0x8F1B_BCDC,
    0xA953_FD4E,
];

/// Added constants of the right line, one per round.
const K_RIGHT: [u32; 5] = [
    0x50A2_8BE6,
    0x5C4D_D124,
    0x6D70_3EF3,
    0x7A6D_76E9,
    0x0000_0000,
];

/// Which message word each step of the left line takes, per round.
const R_LEFT: [[u8; 16]; 5] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5, 2, 14, 11, 8],
    [3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12],
    [1, 9, 11, 10, 0, 8, 12, 4, 13, 3, 7, 15, 14, 5, 6, 2],
    [4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13],
];

/// Which message word each step of the right line takes, per round.
const R_RIGHT: [[u8; 16]; 5] = [
    [5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12],
    [6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12, 4, 9, 1, 2],
    [15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13],
    [8, 6, 4, 1, 3, 11, 15, 0, 5, 12, 2, 13, 9, 7, 10, 14],
    [12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11],
];

/// Left rotation of each step of the left line, per round.
const S_LEFT: [[u32; 16]; 5] = [
    [11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8],
    [7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15, 9, 11, 7, 13, 12],
    [11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5],
    [11, 12, 14, 15, 14, 15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12],
    [9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6],
];

/// Left rotation of each step of the right line, per round.
const S_RIGHT: [[u32; 16]; 5] = [
    [8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6],
    [9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12, 7, 6, 15, 13, 11],
    [9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5],
    [15, 5, 8, 11, 14, 14, 6, 14, 6, 9, 12, 9, 12, 5, 15, 8],
    [8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11],
];

/// The boolean function of a round. The left line uses them in the order
/// 0 to 4, the right line in the order 4 to 0.
#[derive(Clone, Copy)]
enum Mix {
    Xor,
    Choose,
    OrNotXor,
    ChooseByLast,
    XorOrNot,
}

/// The functions of the left line by round.
const MIX_LEFT: [Mix; 5] = [
    Mix::Xor,
    Mix::Choose,
    Mix::OrNotXor,
    Mix::ChooseByLast,
    Mix::XorOrNot,
];

/// The functions of the right line by round.
const MIX_RIGHT: [Mix; 5] = [
    Mix::XorOrNot,
    Mix::ChooseByLast,
    Mix::OrNotXor,
    Mix::Choose,
    Mix::Xor,
];

impl Mix {
    const fn apply(self, x: u32, y: u32, z: u32) -> u32 {
        match self {
            Self::Xor => x ^ y ^ z,
            Self::Choose => (x & y) | (!x & z),
            Self::OrNotXor => (x | !y) ^ z,
            Self::ChooseByLast => (x & z) | (y & !z),
            Self::XorOrNot => x ^ (y | !z),
        }
    }
}

/// A RIPEMD-160 computation in progress.
///
/// Feed the message with [`update`](Self::update) in pieces of any length and
/// take the digest with [`finalize`](Self::finalize). Dropping the hasher,
/// finished or not, overwrites everything it has seen of the message.
pub(crate) struct Ripemd160 {
    /// The chaining state after the blocks processed so far.
    state: [u32; 5],
    /// The bytes of the block that is not complete yet; `filled` of them.
    block: [u8; BLOCK_LEN],
    /// How many bytes of `block` are in use, always below [`BLOCK_LEN`].
    filled: usize,
    /// Length of the message so far in bytes, modulo 2^64.
    length: u64,
    /// The sixteen words of the block being processed. Kept here and not in
    /// a local of the compression function so that it is wiped with the rest.
    schedule: [u32; 16],
}

impl Ripemd160 {
    /// A hasher that has seen no input.
    pub(crate) const fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            block: [0; BLOCK_LEN],
            filled: 0,
            length: 0,
            schedule: [0; 16],
        }
    }

    /// Append `data` to the message.
    pub(crate) fn update(&mut self, mut data: &[u8]) {
        // The specification counts the length modulo 2^64 bits, so wrapping
        // is the defined behaviour, not an overflow to guard against.
        self.length = self.length.wrapping_add(data.len() as u64);

        if self.filled > 0 {
            let space = self.block.iter_mut().skip(self.filled);
            let taken = space.zip(data).fold(0usize, |taken, (slot, byte)| {
                *slot = *byte;
                taken.saturating_add(1)
            });
            self.filled = self.filled.saturating_add(taken);
            data = data.get(taken..).unwrap_or_default();
            if self.filled < BLOCK_LEN {
                return;
            }
            compress(&mut self.state, &mut self.schedule, &self.block);
            self.filled = 0;
        }

        let mut blocks = data.chunks_exact(BLOCK_LEN);
        for block in blocks.by_ref() {
            compress(&mut self.state, &mut self.schedule, block);
        }
        let rest = blocks.remainder();
        for (slot, byte) in self.block.iter_mut().zip(rest) {
            *slot = *byte;
        }
        self.filled = rest.len();
    }

    /// Finish the message and return its digest. The digest is wiped when it
    /// is dropped; the hasher is wiped on return.
    pub(crate) fn finalize(mut self) -> Zeroizing<[u8; DIGEST_LEN]> {
        // Padding: one 1 bit, 0 bits up to 56 bytes modulo 64, then the
        // message length in bits as 64 bits, little-endian. All of it is
        // built in the hasher's own block so that no copy of the message tail
        // is left anywhere else.
        let bits = self.length.wrapping_mul(8);
        let mut padding = self.block.iter_mut().skip(self.filled);
        if let Some(slot) = padding.next() {
            *slot = 0x80;
        }
        for slot in padding {
            *slot = 0;
        }
        if self.filled >= LENGTH_OFFSET {
            // No room for the length: it goes into a block of its own.
            compress(&mut self.state, &mut self.schedule, &self.block);
            self.block.zeroize();
        }
        let length_field = self.block.iter_mut().skip(LENGTH_OFFSET);
        for (slot, byte) in length_field.zip(bits.to_le_bytes()) {
            *slot = byte;
        }
        compress(&mut self.state, &mut self.schedule, &self.block);

        let mut digest = Zeroizing::new([0u8; DIGEST_LEN]);
        for (out, word) in digest.chunks_exact_mut(4).zip(self.state) {
            for (slot, byte) in out.iter_mut().zip(word.to_le_bytes()) {
                *slot = byte;
            }
        }
        digest
    }
}

impl Zeroize for Ripemd160 {
    /// Overwrite everything the hasher holds. What is left is not a usable
    /// hasher; this is the last thing that happens to one.
    fn zeroize(&mut self) {
        self.state.zeroize();
        self.block.zeroize();
        self.filled.zeroize();
        self.length.zeroize();
        self.schedule.zeroize();
    }
}

impl Drop for Ripemd160 {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for Ripemd160 {}

/// Process one 64-byte block: `state` becomes the chaining state after it.
/// `schedule` is the caller's scratch space for the sixteen message words.
fn compress(state: &mut [u32; 5], schedule: &mut [u32; 16], block: &[u8]) {
    for (word, bytes) in schedule.iter_mut().zip(block.chunks_exact(4)) {
        *word = bytes
            .iter()
            .rev()
            .fold(0, |word, &byte| (word << 8) | u32::from(byte));
    }

    let mut left = *state;
    let mut right = *state;
    let left_rounds = R_LEFT.iter().zip(&S_LEFT).zip(K_LEFT).zip(MIX_LEFT);
    let right_rounds = R_RIGHT.iter().zip(&S_RIGHT).zip(K_RIGHT).zip(MIX_RIGHT);
    for (((words, shifts), constant), mix) in left_rounds {
        for (&word, &shift) in words.iter().zip(shifts) {
            step(
                &mut left,
                mix,
                message_word(schedule, word),
                constant,
                shift,
            );
        }
    }
    for (((words, shifts), constant), mix) in right_rounds {
        for (&word, &shift) in words.iter().zip(shifts) {
            step(
                &mut right,
                mix,
                message_word(schedule, word),
                constant,
                shift,
            );
        }
    }

    // h0' = h1 + C + D', h1' = h2 + D + E', and so on round the five words.
    let mut next = [0u32; 5];
    let chained = state.iter().cycle().skip(1);
    let from_left = left.iter().cycle().skip(2);
    let from_right = right.iter().cycle().skip(3);
    for (out, ((h, l), r)) in next.iter_mut().zip(chained.zip(from_left).zip(from_right)) {
        *out = h.wrapping_add(*l).wrapping_add(*r);
    }
    *state = next;

    // The working variables are a function of the message too.
    left.zeroize();
    right.zeroize();
    next.zeroize();
}

/// Word `index` of the schedule. Every index in the tables is below 16, so
/// the fallback is never taken; it is there instead of an index that could
/// panic.
fn message_word(schedule: &[u32; 16], index: u8) -> u32 {
    schedule.get(usize::from(index)).copied().unwrap_or(0)
}

/// One step of either line on its five working variables `[A, B, C, D, E]`.
fn step(line: &mut [u32; 5], mix: Mix, word: u32, constant: u32, shift: u32) {
    let [a, b, c, d, e] = *line;
    let t = a
        .wrapping_add(mix.apply(b, c, d))
        .wrapping_add(word)
        .wrapping_add(constant)
        .rotate_left(shift)
        .wrapping_add(e);
    *line = [e, t, b, c.rotate_left(10), d];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn digest(message: &[u8]) -> String {
        let mut hasher = Ripemd160::new();
        hasher.update(message);
        hex(&hasher.finalize()[..])
    }

    /// The message the length tests hash: byte `i` is `i mod 251`.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn published_test_vectors() {
        // Appendix B of the RIPEMD-160 paper.
        let vectors: [(&[u8], &str); 8] = [
            (b"", "9c1185a5c5e9fc54612808977ee8f548b2258d31"),
            (b"a", "0bdc9d2d256b3ee9daae347be6f4dc835a467ffe"),
            (b"abc", "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc"),
            (
                b"message digest",
                "5d0689ef49d2fae572b881b123a85ffa21595f36",
            ),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "f71c27109c692c1b56bbdceb5b9d2865b3708dbc",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "12a053384a9c0c88e405a06c27dcf49ada62eb2b",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "b0e20b6e3116640286ed3a87a5713079b21f5189",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "9b752e45573d4b39f4dbd3323cab82bf63326bfb",
            ),
        ];
        for (message, expected) in vectors {
            assert_eq!(digest(message), expected, "{} bytes", message.len());
        }
    }

    #[test]
    fn one_million_times_a() {
        // The last vector of appendix B, fed in pieces that do not line up
        // with the blocks.
        let piece = [b'a'; 1000];
        let mut hasher = Ripemd160::new();
        for _ in 0..1000 {
            hasher.update(&piece);
        }
        assert_eq!(
            hex(&hasher.finalize()[..]),
            "52783243c1697bdbe16d37f97f68f08325dc1528"
        );
    }

    #[test]
    fn lengths_around_the_block_boundaries() {
        // 55 bytes are the most that fit into one block with the padding, 56
        // need a second block for the length, 64 fill a block exactly; 119
        // and 120 are the same two cases one block further on. The digests
        // were computed with two independent implementations (the RustCrypto
        // `ripemd` crate and OpenSSL).
        let vectors = [
            (55, "3c86963b3ff646a65ae42996e9664c747cc7e5e6"),
            (56, "ebdd79cfd4fd9949ef8089673d2620427f487cfb"),
            (63, "6d31d3d634b4a7aa15914c239576eb1956f2d9a4"),
            (64, "2581f5e9f957b44b0fa24d31996de47409dd1e0f"),
            (65, "109949b95341eeea7365e8ac4d0d3883d98f709a"),
            (119, "ad430b4283203a7b7f338b9d252dfdbf807402bf"),
            (120, "b89cdc109009f1982c8b34fca446953584d3f6c4"),
        ];
        for (len, expected) in vectors {
            assert_eq!(digest(&pattern(len)), expected, "{len} bytes");
        }
    }

    #[test]
    fn the_digest_does_not_depend_on_how_the_message_is_cut() {
        for len in [0, 1, 55, 56, 63, 64, 65, 119, 120, 128, 129, 300] {
            let message = pattern(len);
            let whole = digest(&message);
            for piece in [1, 3, 7, 55, 63, 64, 65, 200] {
                let mut hasher = Ripemd160::new();
                for part in message.chunks(piece) {
                    hasher.update(part);
                    // An empty piece changes nothing.
                    hasher.update(&[]);
                }
                assert_eq!(
                    hex(&hasher.finalize()[..]),
                    whole,
                    "{len} bytes in pieces of {piece}"
                );
            }
        }
    }

    #[test]
    fn a_dropped_hasher_holds_nothing_of_the_message() {
        // `Drop` does nothing but call `zeroize`, which can be observed on
        // a hasher that is still alive: after it, no field holds anything
        // derived from the input.
        let mut hasher = Ripemd160::new();
        hasher.update(&[0xA5; 100]);
        assert_ne!(hasher.state, INITIAL_STATE);
        assert!(hasher.block.contains(&0xA5));
        assert!(hasher.schedule.contains(&0xA5A5_A5A5));
        hasher.zeroize();
        assert_eq!(hasher.state, [0; 5]);
        assert_eq!(hasher.block, [0; BLOCK_LEN]);
        assert_eq!(hasher.schedule, [0; 16]);
        assert_eq!((hasher.filled, hasher.length), (0, 0));
    }
}
