//! SHA-256 digests and the builder that feeds them unambiguously.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::canonical;

/// A SHA-256 digest, rendered as 64 lowercase hexadecimal characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

/// Why a string is not a valid [`Digest`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestParseError {
    /// A digest is exactly 64 hexadecimal characters.
    #[error("a digest is 64 hex characters, found {found} bytes")]
    WrongLength {
        /// Length of the rejected input in bytes.
        found: usize,
    },
    /// Only lowercase `0-9a-f` is accepted, so every digest has one spelling.
    #[error("byte {position} is not a lowercase hex digit")]
    NotHex {
        /// Offset of the first offending byte.
        position: usize,
    },
}

impl Digest {
    /// The all-zero digest, occupying a checkpoint's id slot only while its real id
    /// is being computed from the other fields.
    pub(crate) const ZERO: Self = Self([0; 32]);

    /// The raw 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The 64-character lowercase hex form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// Parses the form produced by [`Digest::to_hex`].
    ///
    /// # Errors
    ///
    /// Returns [`DigestParseError`] if the text is not exactly 64 lowercase hex characters.
    pub fn from_hex(text: &str) -> Result<Self, DigestParseError> {
        let raw = text.as_bytes();
        if raw.len() != 64 {
            return Err(DigestParseError::WrongLength { found: raw.len() });
        }
        let mut bytes = [0u8; 32];
        let (pairs, remainder) = raw.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(DigestParseError::WrongLength { found: raw.len() });
        }
        for (index, (slot, pair)) in bytes.iter_mut().zip(pairs).enumerate() {
            let position = index * 2;
            let [high, low] = pair;
            let high = nibble(*high).ok_or(DigestParseError::NotHex { position })?;
            let low = nibble(*low).ok_or(DigestParseError::NotHex {
                position: position + 1,
            })?;
            *slot = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// Feeds structured data into SHA-256 so that distinct structures cannot collide
/// by concatenation: every variable-length item is length-prefixed and every
/// digest starts with a domain string naming what is being hashed.
pub(crate) struct DigestBuilder {
    hasher: Sha256,
}

impl DigestBuilder {
    pub(crate) fn new(domain: &str) -> Self {
        let mut builder = Self {
            hasher: Sha256::new(),
        };
        builder.str(domain);
        builder
    }

    pub(crate) fn bytes(&mut self, data: &[u8]) -> &mut Self {
        self.hasher
            .update(u64::try_from(data.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.hasher.update(data);
        self
    }

    pub(crate) fn str(&mut self, text: &str) -> &mut Self {
        self.bytes(text.as_bytes())
    }

    pub(crate) fn u64(&mut self, number: u64) -> &mut Self {
        self.hasher.update(number.to_be_bytes());
        self
    }

    /// A one-byte discriminant, used for enum variants and `Option` presence.
    pub(crate) fn tag(&mut self, tag: u8) -> &mut Self {
        self.hasher.update([tag]);
        self
    }

    pub(crate) fn value(&mut self, value: &Value) -> &mut Self {
        self.bytes(&canonical::to_vec(value))
    }

    pub(crate) fn digest(&mut self, digest: &Digest) -> &mut Self {
        self.hasher.update(digest.0);
        self
    }

    pub(crate) fn finish(self) -> Digest {
        Digest(self.hasher.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let mut builder = DigestBuilder::new("test");
        builder.str("payload");
        let digest = builder.finish();
        assert_eq!(digest.to_hex().len(), 64);
        assert_eq!(Digest::from_hex(&digest.to_hex()).unwrap(), digest);
    }

    #[test]
    fn parsing_rejects_bad_input() {
        assert_eq!(
            Digest::from_hex("abc"),
            Err(DigestParseError::WrongLength { found: 3 })
        );
        let upper = "A".repeat(64);
        assert_eq!(
            Digest::from_hex(&upper),
            Err(DigestParseError::NotHex { position: 0 })
        );
        let late_bad = format!("{}g{}", "0".repeat(10), "0".repeat(53));
        assert_eq!(
            Digest::from_hex(&late_bad),
            Err(DigestParseError::NotHex { position: 10 })
        );
    }

    #[test]
    fn length_prefixing_prevents_concatenation_collisions() {
        let mut left = DigestBuilder::new("d");
        left.str("ab").str("c");
        let mut right = DigestBuilder::new("d");
        right.str("a").str("bc");
        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn domain_separates_digests() {
        let mut left = DigestBuilder::new("one");
        left.str("x");
        let mut right = DigestBuilder::new("two");
        right.str("x");
        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn serde_uses_the_hex_string() {
        let mut builder = DigestBuilder::new("test");
        builder.u64(7);
        let digest = builder.finish();
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"{}\"", digest.to_hex()));
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), digest);
        assert!(serde_json::from_str::<Digest>("\"nope\"").is_err());
    }
}
