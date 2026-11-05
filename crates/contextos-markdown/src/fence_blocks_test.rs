use super::*;

/// A readable outline of the tree: `md(text)` for a segment, `name[children]` for a fence.
fn outline(text: &str) -> String {
    outline_with_code(text, &[])
}

fn outline_with_code(text: &str, code: &[Range<usize>]) -> String {
    let vocabulary = FenceVocabulary::default();
    let tree = FenceTree::from(FenceInput {
        text,
        code,
        vocabulary: &vocabulary,
    });
    render(text, &tree.0)
}

fn render(text: &str, blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            Block::Markdown(range) => format!("md({:?})", text.get(range.clone()).unwrap_or("?")),
            Block::Fence(fence) => format!("{}[{}]", fence.name, render(text, &fence.children)),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_body_with_no_fences_is_one_segment() {
    assert_eq!(outline("# A\n\ntext\n"), r##"md("# A\n\ntext\n")"##);
}

#[test]
fn a_fence_with_no_blank_lines_around_it_is_still_a_container() {
    assert_eq!(
        outline("before\n:::warning\nCareful.\n:::\nafter\n"),
        r#"md("before\n") warning[md("Careful.\n")] md("after\n")"#
    );
}

#[test]
fn fences_nest_and_each_close_closes_the_innermost_open_fence() {
    assert_eq!(
        outline(":::grid{cols=2}\n:::card\nA\n:::\n:::card\nB\n:::\n:::\n"),
        r#"grid[card[md("A\n")] card[md("B\n")]]"#
    );
}

#[test]
fn a_longer_close_closes_a_fence_opened_with_fewer_colons_but_a_shorter_one_does_not() {
    assert_eq!(outline("::::grid\n:::card\nA\n:::\n::::\n"), r#"grid[card[md("A\n")]]"#);
    assert_eq!(outline(":::note\nA\n::::\n"), r#"note[md("A\n")]"#);
    assert_eq!(outline("::::grid\nA\n:::\n"), r#"md("::::grid\nA\n:::\n")"#);
}

#[test]
fn an_unclosed_fence_is_text_and_a_fence_closed_inside_it_still_counts() {
    assert_eq!(
        outline(":::note\n:::tip\nA\n:::\n"),
        r#"md(":::note\n") tip[md("A\n")]"#
    );
}

#[test]
fn a_self_closing_fence_holds_nothing_and_may_take_one_close_line() {
    assert_eq!(
        outline("a\n:::page-break\n:::\nb\n:::section-break\nc\n"),
        r#"md("a\n") page-break[] md("b\n") section-break[] md("c\n")"#
    );
}

#[test]
fn an_unknown_fence_is_a_container_flagged_by_its_missing_spec() -> Result<(), String> {
    let vocabulary = FenceVocabulary::default();
    let text = ":::details\nx\n:::\n";
    let tree = FenceTree::from(FenceInput {
        text,
        code: &[],
        vocabulary: &vocabulary,
    });
    let [Block::Fence(fence)] = tree.0.as_slice() else {
        return Err(format!("expected one fence, got {tree:?}"));
    };
    assert_eq!(fence.name, "details");
    assert!(fence.spec.is_none());
    Ok(())
}

#[test]
fn an_alias_keeps_its_written_name_and_the_spec_it_stands_for() -> Result<(), String> {
    let vocabulary = FenceVocabulary::default();
    let text = ":::banner\nx\n:::\n";
    let tree = FenceTree::from(FenceInput {
        text,
        code: &[],
        vocabulary: &vocabulary,
    });
    let [Block::Fence(fence)] = tree.0.as_slice() else {
        return Err(format!("expected one fence, got {tree:?}"));
    };
    assert_eq!(fence.name, "banner");
    assert_eq!(fence.spec.as_ref().map(|spec| spec.name.as_str()), Some("hero"));
    Ok(())
}

#[test]
fn lines_inside_code_are_never_fence_lines() {
    let text = "```\n:::note\n```\n:::tip\nx\n:::\n";
    let code_end = "```\n:::note\n```\n".len();
    assert_eq!(
        outline_with_code(
            text,
            &[Range {
                start: 0,
                end: code_end
            }]
        ),
        r#"md("```\n:::note\n```\n") tip[md("x\n")]"#
    );
}

#[test]
fn a_line_indented_four_spaces_or_written_with_a_capital_or_a_space_is_not_a_fence_line() {
    assert_eq!(outline("    :::note\nx\n:::\n"), r#"md("    :::note\nx\n:::\n")"#);
    assert_eq!(outline(":::Note\nx\n:::\n"), r#"md(":::Note\nx\n:::\n")"#);
    assert_eq!(outline("::: note\nx\n:::\n"), r#"md("::: note\nx\n:::\n")"#);
    assert_eq!(outline("   :::note\nx\n   :::\n"), r#"note[md("x\n")]"#);
}

#[test]
fn the_opening_line_carries_its_attributes_and_any_problem_with_it() -> Result<(), String> {
    let vocabulary = FenceVocabulary::default();
    let text = ":::note{title=\"Why\" shade=red} trailing words\nx\n:::\n";
    let tree = FenceTree::from(FenceInput {
        text,
        code: &[],
        vocabulary: &vocabulary,
    });
    let [Block::Fence(fence)] = tree.0.as_slice() else {
        return Err(format!("expected one fence, got {tree:?}"));
    };
    assert_eq!(fence.attributes.values.len(), 1);
    assert_eq!(fence.attributes.problems.len(), 1);
    assert_eq!(fence.problems.len(), 1, "{:?}", fence.problems);
    assert_eq!(fence.range, 0..text.len());
    Ok(())
}

#[test]
fn a_body_without_a_final_newline_keeps_its_last_line() {
    assert_eq!(outline(":::note\nx\n:::"), r#"note[md("x\n")]"#);
    assert_eq!(outline("a\n:::note\nx"), r#"md("a\n:::note\nx")"#);
}
