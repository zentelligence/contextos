use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

/// A folder or file path resolved by the one policy every comparison of host and vault locations uses.
///
/// Resolution follows links as far as the path exists and appends the rest as spelt, and it never yields a Windows
/// verbatim (`\\?\C:\...`) form, so a target and a vault root that name the same folder always compare equal, whatever
/// the platform or the spelling.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResolvedPath(PathBuf);

impl TryFrom<&Path> for ResolvedPath {
    type Error = io::Error;

    /// Resolves `value` against the longest prefix that exists, following links and `..` there, and appends the rest.
    ///
    /// A dangling link is an error rather than a missing tail, so a link cannot redirect a later create.
    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        let mut ancestor = std::path::absolute(value)?;
        let mut tail: Vec<OsString> = Vec::new();
        loop {
            match std::fs::symlink_metadata(&ancestor) {
                Ok(_) => break,
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    match ancestor.components().next_back() {
                        Some(Component::Normal(name)) => tail.push(name.to_os_string()),
                        Some(Component::ParentDir) => tail.push(OsString::from("..")),
                        Some(Component::CurDir) => {}
                        Some(Component::RootDir | Component::Prefix(_)) | None => return Err(source),
                    }
                    if !ancestor.pop() {
                        return Err(source);
                    }
                }
                Err(source) => return Err(source),
            }
        }
        let mut resolved = dunce::canonicalize(&ancestor)?;
        for name in tail.into_iter().rev() {
            if name == ".." {
                resolved.pop();
            } else {
                resolved.push(name);
            }
        }
        Ok(Self(dunce::simplified(&resolved).to_path_buf()))
    }
}

impl<'a> From<&'a ResolvedPath> for &'a Path {
    fn from(value: &'a ResolvedPath) -> Self {
        &value.0
    }
}

impl ResolvedPath {
    /// The folder holding this path, which is already resolved because its child was; `None` at a filesystem root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0.parent().map(|parent| Self(parent.to_path_buf()))
    }

    /// Whether `other` is this path or lies inside it, compared by whole components.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }
}

#[cfg(test)]
#[path = "resolved_test.rs"]
mod tests;
