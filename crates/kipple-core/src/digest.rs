//! Digests that tell whether something kipple recorded has changed since.
//!
//! A [`Digest`] is the 128-bit FNV-1a hash of a canonical byte form. It detects change; it
//! is not a security boundary. Every digest kipple stores fails closed: a different
//! digest only ever drops an item or leaves an outcome indeterminate, never authorizes.

use std::fmt;

const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// A 128-bit digest of a canonical byte form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub u128);

impl Digest {
    /// The digest of `value`'s `Debug` form.
    ///
    /// For records kipple compares with themselves later (a policy, a rule), where any
    /// change, even one only of how kipple prints the value, must count as a change. A new
    /// kipple version that prints a value differently makes an older digest differ, which
    /// drops the item: it never makes a changed value look unchanged.
    #[must_use]
    pub fn of_debug(value: &impl fmt::Debug) -> Self {
        let mut hasher = Hasher::new();
        if fmt::write(&mut hasher, format_args!("{value:?}")).is_err() {
            // Only a `Debug` impl can fail here. What it wrote so far is kept, and the
            // marker makes the digest differ from that of a value that printed in full.
            hasher.bytes(b"\xffdebug failed");
        }
        hasher.finish()
    }
}

impl Digest {
    /// The digest of `bytes`, exactly as they are stored. What a stored record's integrity
    /// is checked with: it never depends on how kipple prints anything.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.bytes(bytes);
        hasher.finish()
    }

    /// The digest written as 32 lowercase hexadecimal digits, as [`Display`] writes it.
    ///
    /// [`Display`]: fmt::Display
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let canonical = text.len() == 32
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        canonical
            .then(|| u128::from_str_radix(text, 16).ok())
            .flatten()
            .map(Self)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// Builds a [`Digest`] from bytes fed in order.
#[derive(Debug, Clone)]
pub(crate) struct Hasher(u128);

impl Hasher {
    pub(crate) const fn new() -> Self {
        Self(OFFSET)
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u128::from(*byte);
            self.0 = self.0.wrapping_mul(PRIME);
        }
    }

    pub(crate) const fn finish(&self) -> Digest {
        Digest(self.0)
    }
}

impl fmt::Write for Hasher {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.bytes(s.as_bytes());
        Ok(())
    }
}

/// A SHA-256 digest (FIPS 180-4) of a canonical byte form.
///
/// For what a profile freezes about a rule and its integration (D-068): a digest a rule
/// pack's author can't match with another rule on purpose, which the 128-bit FNV-1a
/// [`Digest`] doesn't promise. Like every digest kipple stores, a different one only ever
/// suspends or drops, never authorizes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256(pub(crate) [u8; 32]);

impl Sha256 {
    /// The digest of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256Hasher::new();
        hasher.bytes(bytes);
        hasher.finish()
    }

    /// The digest written as 64 lowercase hexadecimal digits, as [`Display`] writes it.
    ///
    /// [`Display`]: fmt::Display
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != 64 {
            return None;
        }
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let mut out = [0; 32];
        for (slot, [high, low]) in out.iter_mut().zip(digits.as_chunks::<2>().0) {
            *slot = (nibble(*high)? << 4) | nibble(*low)?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({self})")
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// The first 32 bits of the fractional parts of the cube roots of the first 64 primes.
const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The first 32 bits of the fractional parts of the square roots of the first 8 primes.
const START: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Builds a [`Sha256`] from bytes fed in order.
#[derive(Debug, Clone)]
pub(crate) struct Sha256Hasher {
    state: [u32; 8],
    /// Bytes fed that don't fill a block yet.
    pending: Vec<u8>,
    /// Every byte fed.
    length: u64,
}

impl Sha256Hasher {
    pub(crate) const fn new() -> Self {
        Self {
            state: START,
            pending: Vec::new(),
            length: 0,
        }
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        self.pending.extend_from_slice(bytes);
        let whole = self.pending.len() / 64 * 64;
        let blocks: Vec<u8> = self.pending.drain(..whole).collect();
        for block in blocks.as_chunks::<64>().0 {
            self.compress(block);
        }
    }

    pub(crate) fn finish(mut self) -> Sha256 {
        let bits = self.length.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.pending);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_be_bytes());
        for block in tail.as_chunks::<64>().0 {
            self.compress(block);
        }
        let mut out = [0; 32];
        for (slot, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *slot = word.to_be_bytes();
        }
        Sha256(out)
    }

    /// Mixes one 64-byte block into the state.
    #[expect(
        clippy::indexing_slicing,
        reason = "the schedule has 64 words and every index is between 0 and 63"
    )]
    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0u32; 64];
        for (word, bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for at in 16..64 {
            let early = schedule[at - 15];
            let late = schedule[at - 2];
            let s0 = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
            let s1 = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
            schedule[at] = schedule[at - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[at - 7])
                .wrapping_add(s1);
        }
        // The working variables, a to h in the standard's names.
        let mut work = self.state;
        for (round, word) in ROUND.iter().zip(schedule) {
            let [first, second, third, fourth, fifth, sixth, seventh, last] = work;
            let s1 = fifth.rotate_right(6) ^ fifth.rotate_right(11) ^ fifth.rotate_right(25);
            let choice = (fifth & sixth) ^ (!fifth & seventh);
            let t1 = last
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(*round)
                .wrapping_add(word);
            let s0 = first.rotate_right(2) ^ first.rotate_right(13) ^ first.rotate_right(22);
            let majority = (first & second) ^ (first & third) ^ (second & third);
            let t2 = s0.wrapping_add(majority);
            work = [
                t1.wrapping_add(t2),
                first,
                second,
                third,
                fourth.wrapping_add(t1),
                fifth,
                sixth,
                seventh,
            ];
        }
        for (state, mixed) in self.state.iter_mut().zip(work) {
            *state = state.wrapping_add(mixed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a SHA-256 that differs from the standard's: the FIPS 180-4 examples (one
    /// block, two blocks, empty) and a million `a`s fed in uneven pieces, which crosses
    /// every block boundary with bytes left over.
    #[test]
    fn sha256_matches_the_standards_examples() {
        let cases: [(&[u8], &str); 3] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (bytes, hex) in cases {
            assert_eq!(Sha256::of(bytes).to_string(), hex);
        }
        let mut hasher = Sha256Hasher::new();
        let million = vec![b'a'; 1_000_000];
        for piece in million.chunks(997) {
            hasher.bytes(piece);
        }
        assert_eq!(
            hasher.finish().to_string(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Catches a digest that reads back as another, or a non-canonical spelling read.
    #[test]
    fn a_sha256_parses_only_as_written() {
        let digest = Sha256::of(b"kipple");
        assert_eq!(Sha256::parse(&digest.to_string()), Some(digest));
        let upper = digest.to_string().to_uppercase();
        for bad in ["", "00", upper.as_str(), &"g".repeat(64)] {
            assert_eq!(Sha256::parse(bad), None, "{bad}");
        }
    }
}
