use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Error, Refusal, RefusalCode};

const MAX_POLICY_BYTES: u64 = 256 * 1024;

/// A policy file as read once, with the SHA-256 of what it said, so a launch
/// can refuse a file that changed since its preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFile {
    pub path: PathBuf,
    pub content: String,
    pub hash: String,
}

impl PolicyFile {
    /// Reads a regular file of at most 256 KiB. Bytes that are not UTF-8
    /// become U+FFFD, and the hash is of that text.
    pub fn read(path: &Path) -> Result<PolicyFile, Error> {
        let meta = std::fs::metadata(path).map_err(|source| Error::Io { what: "read the policy file", source })?;
        if !meta.is_file() || meta.len() > MAX_POLICY_BYTES {
            return Err(Error::Invalid { what: "policy file", rule: "a regular file of at most 256 KiB" });
        }
        let bytes = std::fs::read(path).map_err(|source| Error::Io { what: "read the policy file", source })?;
        // The file may have grown between the two calls.
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(Error::Invalid { what: "policy file", rule: "a regular file of at most 256 KiB" });
        }
        let content = String::from_utf8_lossy(&bytes).into_owned();
        Ok(PolicyFile { path: path.to_path_buf(), hash: sha256_hex(content.as_bytes()), content })
    }

    pub fn unchanged_since(&self, now: &PolicyFile) -> Result<(), Error> {
        if self.hash == now.hash {
            Ok(())
        } else {
            Err(Error::Refused(Refusal::new(RefusalCode::PolicyChanged, "The policy file changed after it was previewed.", "Preview it again.")))
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
