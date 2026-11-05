//! The workspace's dependency direction, read from `cargo metadata` so that a new edge that breaks it fails here: the
//! package engine depends on the domain core and the filesystem port alone, the MCP server knows nothing of the workflow
//! engine or the web host, and no library crate depends on the MCP server. Only normal and build dependencies count;
//! a development dependency does not ship.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_json::Value;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Each workspace crate with the workspace crates it depends on (normal and build dependencies only).
fn workspace_edges() -> Result<BTreeMap<String, BTreeSet<String>>, BoxError> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let document: Value = serde_json::from_slice(&output.stdout)?;
    let packages = document["packages"].as_array().ok_or("no packages")?;
    let members: BTreeSet<&str> = packages.iter().filter_map(|package| package["name"].as_str()).collect();
    let mut edges = BTreeMap::new();
    for package in packages {
        let name = package["name"].as_str().ok_or("a package has no name")?;
        let dependencies = package["dependencies"]
            .as_array()
            .ok_or("no dependencies")?
            .iter()
            // A development dependency is `"kind": "dev"`; normal and build dependencies are `null` and `"build"`.
            .filter(|dependency| dependency["kind"].as_str() != Some("dev"))
            .filter_map(|dependency| dependency["name"].as_str())
            .filter(|dependency| members.contains(dependency))
            .map(str::to_owned)
            .collect();
        edges.insert(name.to_owned(), dependencies);
    }
    Ok(edges)
}

#[test]
fn the_package_engine_depends_only_on_the_core_and_the_filesystem_among_workspace_crates() -> Result<(), BoxError> {
    let edges = workspace_edges()?;

    let packages = edges
        .get("contextos-packages")
        .ok_or("contextos-packages is not a workspace member")?;

    let allowed: BTreeSet<String> = ["contextos-core", "contextos-fs"].map(str::to_owned).into();
    assert!(
        packages.is_subset(&allowed),
        "contextos-packages may depend on {allowed:?} among workspace crates, and depends on {packages:?}"
    );
    Ok(())
}

#[test]
fn the_mcp_server_depends_on_neither_the_workflow_engine_nor_the_web_host() -> Result<(), BoxError> {
    let edges = workspace_edges()?;

    let mcp = edges
        .get("contextos-mcp")
        .ok_or("contextos-mcp is not a workspace member")?;

    for forbidden in ["contextos-workflows", "contextos-web", "contextos-packages"] {
        assert!(
            !mcp.contains(forbidden),
            "contextos-mcp must not depend on {forbidden}: {mcp:?}"
        );
    }
    Ok(())
}

#[test]
fn no_library_crate_depends_on_the_mcp_server() -> Result<(), BoxError> {
    let edges = workspace_edges()?;

    let dependents: Vec<&String> = edges
        .iter()
        .filter(|(name, dependencies)| name.as_str() != "contextos-mcp" && dependencies.contains("contextos-mcp"))
        .map(|(name, _)| name)
        .collect();

    assert!(
        dependents.is_empty(),
        "these crates depend on contextos-mcp: {dependents:?}"
    );
    Ok(())
}

#[test]
fn the_architecture_check_reads_the_workspace_it_is_part_of() -> Result<(), BoxError> {
    let edges = workspace_edges()?;

    for member in [
        "contextos-core",
        "contextos-packages",
        "contextos-mcp",
        "contextos-web",
        "contextos-workflows",
    ] {
        assert!(
            edges.contains_key(member),
            "{member} is missing from {:?}",
            edges.keys()
        );
    }
    assert!(
        edges["contextos-web"].contains("contextos-packages"),
        "the web host uses the package engine: {:?}",
        edges["contextos-web"]
    );
    Ok(())
}
