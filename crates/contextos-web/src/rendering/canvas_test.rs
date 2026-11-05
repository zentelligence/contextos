use super::*;

fn text_node(id: &str, x: f64, y: f64, text: &str) -> CanvasNode {
    CanvasNode::Text {
        id: id.to_owned(),
        x,
        y,
        width: 160.0,
        height: 70.0,
        text: text.to_owned(),
        color: None,
    }
}

#[test]
fn renders_a_text_node_at_its_given_position_with_no_auto_layout() {
    let nodes = vec![text_node("a1", 40.0, 50.0, "Hello **world**")];
    let mut content = CanvasContent::default();
    content
        .texts
        .insert("a1".to_owned(), "<p>Hello <strong>world</strong></p>".to_owned());
    let svg = render_svg(&nodes, &[], &content);
    assert!(svg.contains("data-id=\"a1\""));
    assert!(svg.contains("x=\"40\""));
    assert!(svg.contains("y=\"50\""));
    assert!(svg.contains("<strong>world</strong>"));
}

#[test]
fn a_text_node_not_yet_rendered_shows_its_text_escaped() {
    let nodes = vec![text_node("a1", 0.0, 0.0, "<b>raw</b>")];
    let svg = render_svg(&nodes, &[], &CanvasContent::default());
    assert!(svg.contains("&lt;b&gt;raw&lt;/b&gt;"), "{svg}");
}

fn file_node(id: &str, file: &str) -> CanvasNode {
    CanvasNode::File {
        id: id.to_owned(),
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 50.0,
        file: file.to_owned(),
        subpath: None,
        color: None,
    }
}

#[test]
fn a_file_node_links_where_its_routes_send_it_and_is_unavailable_when_they_send_it_nowhere() {
    let nodes = vec![file_node("f1", "notes/example.md"), file_node("f2", "loose/other.md")];
    let mut content = CanvasContent::default();
    content
        .files
        .insert("f1".to_owned(), Some("/example-vault/notes/example.md".to_owned()));
    content.files.insert("f2".to_owned(), None);
    let svg = render_svg(&nodes, &[], &content);
    assert!(svg.contains("<a href=\"/example-vault/notes/example.md\">"), "{svg}");
    assert!(
        svg.contains("class=\"canvas-node-text canvas-file-unavailable\""),
        "{svg}"
    );
    assert_eq!(svg.matches("<a href").count(), 1, "{svg}");
}

#[test]
fn renders_a_link_node_as_an_external_preview_never_fetching_it() {
    let nodes = vec![CanvasNode::Link {
        id: "l1".to_owned(),
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 50.0,
        url: "https://jsoncanvas.org/spec/1.0/".to_owned(),
        color: None,
    }];
    let svg = render_svg(&nodes, &[], &CanvasContent::default());
    assert!(svg.contains("https://jsoncanvas.org/spec/1.0/"));
    assert!(svg.contains("target=\"_blank\""));
}

#[test]
fn renders_a_group_node_beneath_its_members_with_its_label() {
    let nodes = vec![CanvasNode::Group {
        id: "g1".to_owned(),
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 100.0,
        label: Some("Group label".to_owned()),
        background: None,
        background_style: None,
        color: None,
    }];
    let svg = render_svg(&nodes, &[], &CanvasContent::default());
    assert!(svg.contains("canvas-group-label"));
    assert!(svg.contains("Group label"));
}

#[test]
fn renders_an_edge_between_two_node_anchor_points() {
    let nodes = vec![
        text_node("a", 0.0, 0.0, "A"),
        CanvasNode::Text {
            id: "b".to_owned(),
            x: 300.0,
            y: 0.0,
            width: 160.0,
            height: 70.0,
            text: "B".to_owned(),
            color: None,
        },
    ];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: Some("right".to_owned()),
        from_end: None,
        to_node: "b".to_owned(),
        to_side: Some("left".to_owned()),
        to_end: Some("arrow".to_owned()),
        color: None,
        label: Some("edge label".to_owned()),
    }];
    let svg = render_svg(&nodes, &edges, &CanvasContent::default());
    assert!(svg.contains("canvas-edge"));
    assert!(svg.contains("marker-end"));
    assert!(svg.contains("edge label"));
}

#[test]
fn an_edge_with_to_end_none_carries_no_arrowhead_marker() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A"), text_node("b", 300.0, 0.0, "B")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "b".to_owned(),
        to_side: None,
        to_end: Some("none".to_owned()),
        color: None,
        label: None,
    }];
    let svg = render_svg(&nodes, &edges, &CanvasContent::default());
    assert!(!svg.contains("marker-end"));
}

#[test]
fn an_edge_referencing_a_missing_node_is_skipped_not_a_panic() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "does-not-exist".to_owned(),
        to_side: None,
        to_end: None,
        color: None,
        label: None,
    }];
    let svg = render_svg(&nodes, &edges, &CanvasContent::default());
    assert!(!svg.contains("canvas-edge"));
}

#[test]
fn rendering_is_deterministic_across_repeated_calls() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A"), text_node("b", 300.0, 0.0, "B")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "b".to_owned(),
        to_side: None,
        to_end: None,
        color: None,
        label: None,
    }];
    let first = render_svg(&nodes, &edges, &CanvasContent::default());
    let second = render_svg(&nodes, &edges, &CanvasContent::default());
    assert_eq!(first, second);
}

#[test]
fn a_preset_colour_resolves_to_a_design_hue_by_its_conventional_meaning() {
    // JSON Canvas preset 1 is conventionally red; 4 green; 6 purple. The
    // values are drawn from the design system's hue family and are fixed in
    // both skins, because a diagram's interior keeps its colours in either.
    assert_eq!(resolve_color(Some("1"), "#000"), "#F4586B");
    assert_eq!(resolve_color(Some("4"), "#000"), "#5FE393");
    assert_eq!(resolve_color(Some("6"), "#000"), "#B49CFB");
}

#[test]
fn an_unknown_preset_string_passes_through_rather_than_panicking() {
    assert_eq!(resolve_color(Some("7"), "#fallback"), "7");
    assert_eq!(resolve_color(Some("crimson"), "#fallback"), "crimson");
}

#[test]
fn a_literal_hex_colour_passes_through_unchanged() {
    assert_eq!(resolve_color(Some("#123abc"), "#000"), "#123abc");
}

#[test]
fn no_colour_uses_the_fallback() {
    assert_eq!(resolve_color(None, "#fallback"), "#fallback");
}
