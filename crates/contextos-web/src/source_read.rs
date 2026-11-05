//! Reading a file of a vault's source, with every way the read can go told apart.
//!
//! A file that is not there is missing. A file that is there but that the server will not hand over, because it is too
//! large, is a link out of the vault, or cannot be read, is a defect in the vault and is reported as what it is. A server
//! that cannot be asked, or that answers with something this host does not understand, says nothing about the file at
//! all: that is an outage and never a defect in the vault, so it is an error and not an answer.

use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_content::{AttachedRead, read_attached};

/// What reading one source file came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceRead {
    Text(String),
    /// Nothing is at the path.
    Missing,
    /// The file is there and cannot be used, for the reason given as a clause that follows its name.
    Unusable(&'static str),
}

/// What the stable code of a refused read says about the file.
enum Refusal {
    NothingThere,
    TheFile(&'static str),
    NotTheFile,
}

impl From<Option<&str>> for Refusal {
    fn from(code: Option<&str>) -> Self {
        match code {
            Some("path/not-found" | "path/not-a-directory") => Self::NothingThere,
            Some("io/too-large") => Self::TheFile("is larger than the server will read"),
            Some("path/outside-root" | "path/symlink-escape") => {
                Self::TheFile("is a link out of the vault, which the server does not follow")
            }
            Some("io/binary") => Self::TheFile("is not text"),
            Some("io/filesystem" | "io/unreadable") => Self::TheFile("could not be read"),
            _ => Self::NotTheFile,
        }
    }
}

/// Reads the text of `path`.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails, and [`McpCallError::Refused`] when the server
/// refused for a reason that says nothing about the file, such as being busy or not knowing the vault.
pub(crate) async fn read_source(client: &McpClient, path: &str) -> Result<SourceRead, McpCallError> {
    Ok(match read_attached(client, path).await? {
        AttachedRead::Text(read) => SourceRead::Text(read.text),
        AttachedRead::NotText => SourceRead::Unusable("is not text"),
        AttachedRead::Refused { code } => match Refusal::from(code.as_deref()) {
            Refusal::NothingThere => SourceRead::Missing,
            Refusal::TheFile(reason) => SourceRead::Unusable(reason),
            Refusal::NotTheFile => return Err(unexpected_answer(client)),
        },
    })
}

/// The error for a server that answered, but not with what was asked for.
pub(crate) fn unexpected_answer(client: &McpClient) -> McpCallError {
    McpCallError::Refused {
        server: client.name().to_owned(),
        source: Box::new(rmcp::service::ServiceError::UnexpectedResponse),
    }
}

#[cfg(test)]
#[path = "source_read_test.rs"]
mod tests;
