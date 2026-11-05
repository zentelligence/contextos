use contextos_core::ContentHash;
use sha2::{Digest, Sha256};

/// The SHA-256 content hash of `bytes`, in the form every other hash in the product takes.
pub(crate) fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from(<[u8; 32]>::from(Sha256::digest(bytes)))
}
