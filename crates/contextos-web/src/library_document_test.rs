use serde_json::json;

use super::*;

#[test]
fn properties_keep_their_order_and_flatten_lists() -> Result<(), Box<dyn std::error::Error>> {
    let frontmatter = json!({
        "name": "alex",
        "tools": ["read", "write"],
        "count": 3,
        "empty": null,
        "blank": ""
    });
    let frontmatter = frontmatter.as_object().ok_or("not an object")?;
    let shown = properties(frontmatter);
    let keys: Vec<&str> = shown.iter().map(|property| property.key.as_str()).collect();
    assert_eq!(keys, ["name", "tools", "count", "empty", "blank"]);
    assert_eq!(shown[1].values, ["read", "write"]);
    assert_eq!(shown[2].values, ["3"]);
    assert_eq!(shown[3].values, Vec::<String>::new());
    assert_eq!(shown[4].values, Vec::<String>::new());
    Ok(())
}

#[test]
fn the_frontmatter_block_is_found_only_at_the_very_start() {
    assert_eq!(frontmatter_block("---\na: 1\n---\n# Body\n"), Some("---\na: 1\n---\n"));
    assert_eq!(frontmatter_block("# Body\n---\na: 1\n---\n"), None);
    assert_eq!(frontmatter_block("---\nunterminated\n"), None);
}

#[test]
fn a_file_with_any_carriage_return_is_converted_when_saved() {
    assert_eq!(LineBreaks::from("a\nb\n"), LineBreaks::Unix);
    assert_eq!(LineBreaks::from("single line"), LineBreaks::Unix);
    assert_eq!(LineBreaks::from("a\r\nb\r\n"), LineBreaks::Converted);
    assert_eq!(LineBreaks::from("a\r\nb\n"), LineBreaks::Converted);
    assert_eq!(LineBreaks::from("a\rb\r"), LineBreaks::Converted);
    assert_eq!(LineBreaks::from("ends with\r"), LineBreaks::Converted);
}
