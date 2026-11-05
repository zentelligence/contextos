use contextos_workflows::WorkflowFilePathError;

use super::*;

fn path(raw: &str) -> Result<WorkflowFilePath, WorkflowFilePathError> {
    WorkflowFilePath::try_from(raw)
}

fn tree(json: serde_json::Value) -> Result<TreeNode, serde_json::Error> {
    serde_json::from_value(json)
}

fn listed(found: &[WorkflowFilePath]) -> Vec<&str> {
    found.iter().map(WorkflowFilePath::as_str).collect()
}

#[test]
fn a_nested_folder_lists_every_file_beneath_it() -> Result<(), Box<dyn std::error::Error>> {
    let node = tree(serde_json::json!({"name": "files", "type": "dir", "children": [
        {"name": "a.md", "type": "file"},
        {"name": "deep", "type": "dir", "children": [
            {"name": "b.md", "type": "file"},
            {"name": "empty", "type": "dir", "children": []}
        ]}
    ]}))?;
    let mut found = Vec::new();

    collect(node.children.as_deref(), &path("files")?, 10, &mut found)?;

    assert_eq!(listed(&found), ["files/a.md", "files/deep/b.md"]);
    Ok(())
}

#[test]
fn a_folder_past_the_depth_bound_is_refused_rather_than_listed_in_part() -> Result<(), Box<dyn std::error::Error>> {
    let node = tree(serde_json::json!({"name": "files", "type": "dir", "children": [
        {"name": "deep", "type": "dir"}
    ]}))?;
    let mut found = Vec::new();

    let refused = collect(node.children.as_deref(), &path("files")?, 10, &mut found);

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[test]
fn a_folder_over_the_limit_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let node = tree(serde_json::json!({"name": "files", "type": "dir", "children": [
        {"name": "a.md", "type": "file"}, {"name": "b.md", "type": "file"}, {"name": "c.md", "type": "file"}
    ]}))?;
    let mut found = Vec::new();

    let refused = collect(node.children.as_deref(), &path("files")?, 2, &mut found);

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[test]
fn a_name_no_workflow_path_can_address_fails_the_listing() -> Result<(), Box<dyn std::error::Error>> {
    let node = tree(serde_json::json!({"name": "files", "type": "dir", "children": [
        {"name": "notes:2024.md", "type": "file"}
    ]}))?;
    let mut found = Vec::new();

    let refused = collect(node.children.as_deref(), &path("files")?, 10, &mut found);

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileUnreadable)
    );
    Ok(())
}

#[test]
fn a_missing_path_is_not_found_and_any_other_refusal_is_unreadable_with_its_code()
-> Result<(), Box<dyn std::error::Error>> {
    let file = path("files/a.md")?;

    let missing = refused(&file, Some("path/not-found"));
    let escaping = refused(&file, Some("path/symlink-escape"));
    let unexplained = refused(&file, None);

    assert_eq!(missing.kind, WorkflowFailureKind::FileNotFound);
    assert_eq!(escaping.kind, WorkflowFailureKind::FileUnreadable);
    assert!(escaping.message.contains("path/symlink-escape"), "{}", escaping.message);
    assert_eq!(unexplained.kind, WorkflowFailureKind::FileUnreadable);
    Ok(())
}
