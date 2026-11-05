use super::*;

#[test]
fn a_markdown_definition_name_gains_its_extension_once() {
    assert_eq!(
        new_file_name(DefinitionLayout::Markdown, "critic"),
        Ok("critic.md".to_owned())
    );
    assert_eq!(
        new_file_name(DefinitionLayout::Markdown, " critic.md "),
        Ok("critic.md".to_owned())
    );
}

#[test]
fn a_script_name_must_carry_its_own_extension() {
    assert_eq!(
        new_file_name(DefinitionLayout::AnyFile, "tidy.py"),
        Ok("tidy.py".to_owned())
    );
    assert!(new_file_name(DefinitionLayout::AnyFile, "tidy").is_err());
    assert!(new_file_name(DefinitionLayout::AnyFile, "tidy.").is_err());
}

#[test]
fn a_skill_is_created_as_a_folder_holding_its_entry_file() {
    assert_eq!(
        new_file_name(DefinitionLayout::Folder { entry: "SKILL.md" }, "plan-week"),
        Ok("plan-week/SKILL.md".to_owned())
    );
    assert!(new_file_name(DefinitionLayout::Folder { entry: "SKILL.md" }, "plan.week").is_err());
}

#[test]
fn a_new_name_cannot_traverse_hide_or_take_a_reserved_segment() {
    for name in ["", "  ", "../escape", "a/b", "a\\b", ".hidden", "new", "edit", ".."] {
        assert!(new_file_name(DefinitionLayout::Markdown, name).is_err(), "{name:?}");
    }
}

fn tree() -> VaultFolder {
    VaultFolder {
        name: "prompts".to_owned(),
        relative_path: String::new(),
        folders: vec![VaultFolder {
            name: "writing".to_owned(),
            relative_path: "writing".to_owned(),
            folders: Vec::new(),
            files: vec![VaultFile {
                name: "email.md".to_owned(),
                relative_path: "writing/email.md".to_owned(),
            }],
        }],
        files: vec![VaultFile {
            name: "framework.md".to_owned(),
            relative_path: "framework.md".to_owned(),
        }],
    }
}

#[test]
fn a_path_resolves_to_what_the_listing_actually_holds() {
    let root = tree();
    assert!(matches!(resolve(Some(&root), "", false), Target::Folder(path) if path.is_empty()));
    assert!(matches!(resolve(Some(&root), "writing", false), Target::Folder(path) if path == "writing"));
    assert!(
        matches!(resolve(Some(&root), "writing/email.md", false), Target::File(path) if path == "writing/email.md")
    );
    assert!(
        matches!(resolve(Some(&root), "writing/email.md/edit", false), Target::Edit(path) if path == "writing/email.md")
    );
    assert!(matches!(resolve(Some(&root), "writing/new", false), Target::New(path) if path == "writing"));
    assert!(matches!(resolve(Some(&root), "new", false), Target::New(path) if path.is_empty()));
    assert!(matches!(resolve(Some(&root), "missing.md", false), Target::Missing));
    assert!(matches!(
        resolve(Some(&root), "writing/../framework.md", false),
        Target::Missing
    ));
    assert!(matches!(
        resolve(Some(&root), "framework.md/new", false),
        Target::Missing
    ));
}

#[test]
fn edit_at_a_registry_types_root_names_its_locations_not_a_file() {
    let root = tree();
    assert!(matches!(resolve(Some(&root), "edit", true), Target::TypeSettings));
    assert!(matches!(resolve(Some(&root), "edit", false), Target::Missing));
    assert!(matches!(resolve(None, "edit", true), Target::TypeSettings));
    assert!(matches!(resolve(None, "anything.md", true), Target::Missing));
}
