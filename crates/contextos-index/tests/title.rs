use contextos_index::{IndexTitle, IndexTitleInput};

#[test]
fn derives_title_from_frontmatter_title() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Better Title\n---\nBody.\n";

    let title = IndexTitle::try_from(IndexTitleInput {
        filename: "fallback-name.md",
        source: Some(source),
    })?;

    assert_eq!(<&str>::from(&title), "Better Title");
    Ok(())
}

#[test]
fn humanises_the_filename_when_frontmatter_title_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let title = IndexTitle::try_from(IndexTitleInput {
        filename: "client-project_status.md",
        source: Some("Body with no frontmatter.\n"),
    })?;

    assert_eq!(<&str>::from(&title), "Client Project Status");
    Ok(())
}

#[test]
fn humanises_the_filename_when_there_is_no_source_at_all() -> Result<(), Box<dyn std::error::Error>> {
    let title = IndexTitle::try_from(IndexTitleInput {
        filename: "sub-folder",
        source: None,
    })?;

    assert_eq!(<&str>::from(&title), "Sub Folder");
    Ok(())
}

#[test]
fn humanises_the_filename_when_the_frontmatter_title_is_blank() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: \"   \"\n---\nBody.\n";

    let title = IndexTitle::try_from(IndexTitleInput {
        filename: "fallback-name.md",
        source: Some(source),
    })?;

    assert_eq!(<&str>::from(&title), "Fallback Name");
    Ok(())
}
