use serde_json::json;

use super::*;

fn root_from(value: serde_json::Value) -> Result<VaultFolder, Box<dyn std::error::Error>> {
    let node: DirectoryTreeNode = serde_json::from_value(value)?;
    Ok(VaultFolder::from(&node))
}

fn sample() -> Result<VaultFolder, Box<dyn std::error::Error>> {
    root_from(json!({
        "name": "prompts", "type": "dir", "children": [
            {"name": "writing", "type": "dir", "children": [
                {"name": "index.md", "type": "file"},
                {"name": "email-professional.md", "type": "file"}
            ]},
            {"name": "Untitled.md", "type": "file"},
            {"name": "framework.md", "type": "file"},
            {"name": "index.md", "type": "file"},
            {"name": ".obsidian", "type": "dir", "children": [{"name": "app.json", "type": "file"}]},
            {"name": "__pycache__", "type": "dir", "children": [{"name": "x.pyc", "type": "file"}]},
            {"name": "notes.pyc", "type": "file"},
            {"name": "analysis", "type": "dir", "children": []}
        ]
    }))
}

#[test]
fn folders_and_files_sort_by_name_and_no_managed_index_is_listed() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    let folders: Vec<&str> = root.folders.iter().map(|folder| folder.name.as_str()).collect();
    assert_eq!(folders, ["analysis", "writing"]);
    let files: Vec<&str> = root.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(files, ["framework.md", "Untitled.md"]);
    Ok(())
}

#[test]
fn hidden_entries_and_compiled_python_caches_are_never_listed() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    assert!(
        root.folders
            .iter()
            .all(|folder| !folder.name.starts_with('.') && folder.name != "__pycache__")
    );
    assert!(root.files.iter().all(|file| file.extension().as_deref() != Some("pyc")));
    Ok(())
}

#[test]
fn relative_paths_are_built_from_the_listed_root() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    let writing = root.find_folder("writing").ok_or("no writing folder")?;
    assert_eq!(writing.relative_path, "writing");
    let email = root.find_file("writing/email-professional.md").ok_or("no email file")?;
    assert_eq!(email.relative_path, "writing/email-professional.md");
    assert_eq!(email.stem(), "email-professional");
    assert!(email.is_markdown());
    Ok(())
}

#[test]
fn lookups_never_match_a_missing_or_partial_path() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    assert!(root.find_file("writing").is_none());
    assert!(root.find_folder("writing/email-professional.md").is_none());
    assert!(root.find_file("missing.md").is_none());
    assert!(
        root.find_folder("")
            .is_some_and(|folder| folder.relative_path.is_empty())
    );
    Ok(())
}

#[test]
fn counting_never_meets_an_index_file() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    assert_eq!(root.count_files(&|file: &VaultFile| !file.is_index()), 3);
    assert_eq!(root.count_files(&|_: &VaultFile| true), 3);
    Ok(())
}

#[test]
fn walking_visits_every_file_in_display_order() -> Result<(), Box<dyn std::error::Error>> {
    let root = sample()?;
    let paths: Vec<&str> = root
        .files_depth_first()
        .map(|file| file.relative_path.as_str())
        .collect();
    assert_eq!(paths, ["framework.md", "Untitled.md", "writing/email-professional.md"]);
    Ok(())
}
