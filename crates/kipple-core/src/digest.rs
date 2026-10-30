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
