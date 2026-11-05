//! A vault library's `library.toml`: the modules it ships, the ones a workflow may import, the libraries its modules
//! import, and the tools its functions call.

use serde::Deserialize;

use crate::call_rules::{ANY_TOOL, ToolRef};
use crate::library_requirements::{LibraryRequirements, PackageFileOrigin, SourceText};
use crate::package::{DependencyRaw, PackageError, RawExports};
use crate::script_modules::DeclaredModules;

/// What `library.toml` declares, checked. The library's name is the package's, never a key in the file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryManifest {
    modules: DeclaredModules,
    exports: DeclaredModules,
    libraries: LibraryRequirements,
    mcp_tools: Vec<ToolRef>,
}

/// `library.toml` as written. Unknown keys are refused, as in `workflow.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryManifestRaw {
    modules: Vec<String>,
    exports: Vec<String>,
    #[serde(default)]
    libraries: Vec<DependencyRaw>,
    #[serde(default)]
    mcp_tools: Vec<LibraryToolRaw>,
}

/// One tool the library calls, or `*` for every tool on a server: a declaration only, with no allow or deny, since a
/// library grants nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryToolRaw {
    server: String,
    tool: String,
}

impl TryFrom<&str> for LibraryManifest {
    type Error = PackageError;

    /// The `library.toml` of an installed release, in which every library states its requirement.
    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::try_from((text, PackageFileOrigin::Release))
    }
}

impl TryFrom<SourceText<'_>> for LibraryManifest {
    type Error = PackageError;

    /// The `library.toml` of a vault's source, in which a library may state no requirement.
    fn try_from(source: SourceText<'_>) -> Result<Self, Self::Error> {
        Self::try_from((source.0, PackageFileOrigin::Source))
    }
}

impl TryFrom<(&str, PackageFileOrigin)> for LibraryManifest {
    type Error = PackageError;

    fn try_from((text, origin): (&str, PackageFileOrigin)) -> Result<Self, Self::Error> {
        let invalid = |reason: String| PackageError::InvalidLibraryManifest { reason };
        let raw: LibraryManifestRaw = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
        let modules = DeclaredModules::try_from(raw.modules).map_err(|error| invalid(error.to_string()))?;
        if raw.exports.is_empty() {
            return Err(invalid(
                "`exports` is empty: a library exports at least one module, since nothing could use it otherwise"
                    .to_owned(),
            ));
        }
        let exports = DeclaredModules::try_from(RawExports {
            paths: raw.exports,
            modules: &modules,
        })
        .map_err(|error| invalid(error.to_string()))?;
        let libraries =
            LibraryRequirements::try_from((raw.libraries, origin)).map_err(|error| invalid(error.to_string()))?;
        let mut mcp_tools: Vec<ToolRef> = Vec::with_capacity(raw.mcp_tools.len());
        for entry in raw.mcp_tools {
            if entry.server.trim().is_empty() || entry.tool.trim().is_empty() {
                return Err(invalid(
                    "`mcp_tools` holds an entry with a blank server or tool name".to_owned(),
                ));
            }
            let declared = ToolRef {
                server: entry.server,
                tool: entry.tool,
            };
            if mcp_tools.contains(&declared) {
                return Err(invalid(format!(
                    "`mcp_tools` names {} on server {:?} twice",
                    declared.tool, declared.server
                )));
            }
            mcp_tools.push(declared);
        }
        for entry in mcp_tools.iter().filter(|entry| entry.tool == ANY_TOOL) {
            if mcp_tools
                .iter()
                .any(|other| other.server == entry.server && other.tool != ANY_TOOL)
            {
                return Err(invalid(format!(
                    "`mcp_tools` declares {ANY_TOOL:?} on server {:?} and also names tools on it, which is redundant: \
                     {ANY_TOOL:?} already declares every tool on the server, so remove the other entries for it",
                    entry.server
                )));
            }
        }
        Ok(Self {
            modules,
            exports,
            libraries,
            mcp_tools,
        })
    }
}

impl LibraryManifest {
    /// Every module the library ships.
    #[must_use]
    pub fn modules(&self) -> &DeclaredModules {
        &self.modules
    }

    /// The modules a workflow, or another library, may import.
    #[must_use]
    pub fn exports(&self) -> &DeclaredModules {
        &self.exports
    }

    /// The libraries its modules import from, each with the requirement it declares.
    #[must_use]
    pub fn libraries(&self) -> &LibraryRequirements {
        &self.libraries
    }

    /// The tools its functions call, in the order declared.
    #[must_use]
    pub fn mcp_tools(&self) -> &[ToolRef] {
        &self.mcp_tools
    }
}

#[cfg(test)]
#[path = "library_manifest_test.rs"]
mod tests;
