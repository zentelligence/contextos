use super::*;

fn pieces(source: &str) -> Vec<String> {
    Scanner::from(source)
        .map(|piece| match piece {
            Piece::Text(_) => "text".to_owned(),
            Piece::Wikilink(link) => format!("wikilink@{}:{}", link.line, link.raw),
            Piece::UnclosedWikilink { line, .. } => format!("unclosed@{line}"),
            Piece::Standard(link) => format!("standard@{}:{}", link.line, link.raw),
            Piece::Reference(reference) => format!("reference@{}:{}", reference.line, reference.raw),
            Piece::Definition(definition) => format!("definition:{}", definition.label),
        })
        .filter(|piece| piece != "text")
        .collect()
}

#[test]
fn a_frontmatter_block_is_prose_so_its_indented_lines_are_not_code() {
    assert_eq!(pieces("---\nkey:\n\n    - \"[[a]]\"\n---\n"), ["wikilink@4:[[a]]"]);
}

#[test]
fn a_lone_thematic_break_at_the_top_does_not_open_frontmatter() {
    assert_eq!(pieces("---\n\n    [[code]]\n"), Vec::<String>::new());
}

#[test]
fn the_label_of_a_reference_style_link_is_not_read_as_a_link_of_its_own() {
    assert_eq!(pieces("[text][label]\n"), ["reference@1:[text][label]"]);
}

#[test]
fn a_link_inside_the_text_of_a_reference_style_link_is_still_found() {
    assert_eq!(
        pieces("[see [[a]]][label]\n"),
        ["reference@1:[see [[a]]][label]", "wikilink@1:[[a]]"]
    );
}

#[test]
fn a_definition_line_is_one_piece_and_is_not_scanned_for_links() {
    assert_eq!(pieces("[ref]: <a [[b]].md> \"t\"\n"), ["definition:ref"]);
}

#[test]
fn a_document_of_many_unclosed_backtick_runs_is_scanned_without_searching_it_whole_for_each() {
    let source = "`\n``\n```x\n".repeat(20_000);

    assert!(Scanner::from(source.as_str()).count() > 0);
}
