//! A temporary vault for the read tests (`read_parts.rs`, `read_version.rs`): one root, a configurable read maximum,
//! and helpers to write a file, read it whole, read one part, and read it part by part to the end.
//!
//! Not itself a test target: as a `tests/` subdirectory module it is compiled once per including test crate, and
//! some helpers are used by only one of them.
#![allow(dead_code)]

use std::path::PathBuf;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{
    BytePart, FileVersion, Filesystem, FilesystemConfig, FsError, FsLimits, ReadLimit, ReadPartRequest, ReadPartResult,
    ReadTextRequest, ReadTextResult, default_hidden_patterns,
};
use tempfile::{TempDir, tempdir};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub struct Vault {
    _directory: TempDir,
    pub root: PathBuf,
    pub filesystem: Filesystem,
    roots: VaultSet,
}

impl Vault {
    pub fn with_max_read_bytes(max_read_bytes: u64) -> Result<Self, Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let root = directory.path().to_path_buf();
        let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
            path: root.clone(),
            managed: true,
            name: Some("vault".to_owned()),
        })?])?;
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![FsLimits {
                max_read_bytes,
                max_batch_files: 50,
            }],
            hidden: vec![default_hidden_patterns()],
            atomic_write_guard: None,
        })?;
        Ok(Self {
            _directory: directory,
            root,
            filesystem,
            roots,
        })
    }

    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_max_read_bytes(1024 * 1024)
    }

    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<VaultPath, Box<dyn std::error::Error>> {
        std::fs::write(self.root.join(name), bytes)?;
        self.path(name)
    }

    pub fn path(&self, name: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: name,
        })?)
    }

    pub fn read(&self, path: &VaultPath, limit: Option<ReadLimit>) -> Result<ReadTextResult, FsError> {
        self.filesystem.read_text(&ReadTextRequest {
            path: path.clone(),
            limit,
        })
    }

    pub fn try_part(
        &self,
        path: &VaultPath,
        offset: u64,
        length: u64,
        expected_version: Option<FileVersion>,
    ) -> Result<ReadPartResult, FsError> {
        self.filesystem.read_part(&ReadPartRequest {
            path: path.clone(),
            part: BytePart::from((offset, length)),
            expected_version,
        })
    }

    pub fn part(&self, path: &VaultPath, offset: u64, length: u64) -> Result<ReadPartResult, FsError> {
        self.try_part(path, offset, length, None)
    }

    /// Reads the file part by part from offset 0, following each `next_offset` until it is -1, and joins the
    /// parts, checking on the way that each part starts where the previous one said, holds exactly `length`
    /// bytes, is never longer than asked, and makes progress.
    pub fn read_in_parts(&self, path: &VaultPath, length: u64) -> Result<String, Box<dyn std::error::Error>> {
        let mut joined = String::new();
        let mut offset = 0_u64;
        loop {
            let part = self.part(path, offset, length)?;
            assert_eq!(
                part.offset, offset,
                "each part starts where the previous one said the next would"
            );
            assert_eq!(u64::try_from(part.content.len())?, part.length);
            assert!(part.length <= length, "a part is never longer than asked");
            joined.push_str(&part.content);
            if part.next_offset == -1 {
                return Ok(joined);
            }
            let next = u64::try_from(part.next_offset)?;
            assert!(next > offset, "a part always makes progress");
            assert_eq!(next, offset + part.length, "the next part starts right after this one");
            offset = next;
        }
    }
}
