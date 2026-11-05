//! A host path shown by its last components only, so a message names which file it means without printing the absolute
//! location of the operator's data folder.

use std::fmt;
use std::path::Path;

/// The last `count` normal components of `path`, joined with `/`.
pub(crate) struct Trailing<'a> {
    pub path: &'a Path,
    pub count: usize,
}

impl fmt::Display for Trailing<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<_> = self
            .path
            .components()
            .rev()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(name.to_string_lossy()),
                _ => None,
            })
            .take(self.count)
            .collect();
        names.reverse();
        formatter.write_str(&names.join("/"))
    }
}

#[cfg(test)]
#[path = "trailing_path_test.rs"]
mod tests;
