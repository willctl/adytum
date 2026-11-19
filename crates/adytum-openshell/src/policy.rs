use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::OpenShellError;

/// The largest policy file OpenShell is given.
pub const MAX_POLICY_BYTES: u64 = 256 * 1024;

/// A policy file as read at one moment: where it is, what it said, and the
/// SHA-256 of what it said. A launch reads it again and refuses if the hash
/// moved since the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFile {
    pub path: PathBuf,
    pub content: String,
    pub hash: String,
}

impl PolicyFile {
    /// Reads a regular file of at most 256 KiB. Bytes that are not UTF-8
    /// become U+FFFD, and the hash is of that text.
    pub fn read(path: &Path) -> Result<PolicyFile, OpenShellError> {
        let meta = std::fs::metadata(path).map_err(|source| OpenShellError::PolicyIo { op: "stat", source })?;
        if !meta.is_file() || meta.len() > MAX_POLICY_BYTES {
            return Err(OpenShellError::PolicyTooBig);
        }
        let bytes = std::fs::read(path).map_err(|source| OpenShellError::PolicyIo { op: "open", source })?;
        // The file may have grown between the two calls.
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(OpenShellError::PolicyTooBig);
        }
        let content = String::from_utf8_lossy(&bytes).into_owned();
        Ok(PolicyFile { path: path.to_path_buf(), hash: sha256_hex(content.as_bytes()), content })
    }

    /// Refuses when the file now differs from the one that was previewed.
    pub fn unchanged_since(&self, now: &PolicyFile) -> Result<(), OpenShellError> {
        if self.hash == now.hash {
            Ok(())
        } else {
            Err(OpenShellError::PolicyChanged)
        }
    }
}

/// Lower-case hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
