use super::*;
use crate::FenceVocabulary;

fn parse(raw: &str) -> FenceAttributes {
    FenceAttributes::from(AttributeInput {
        raw,
        vocabulary: &FenceVocabulary::default(),
    })
}

#[test]
fn quoted_bare_and_flag_attributes_are_read_by_their_types() {
    let parsed = parse(r#"title="Why \"this\" matters" cols=3 collapse open=false variant=compact"#);
    assert_eq!(
        parsed.values,
        vec![
            (
                "title".to_owned(),
                AttributeValue::Text("Why \"this\" matters".to_owned())
            ),
            ("cols".to_owned(), AttributeValue::Integer(3)),
            ("collapse".to_owned(), AttributeValue::Boolean(true)),
            ("open".to_owned(), AttributeValue::Boolean(false)),
            ("variant".to_owned(), AttributeValue::Token("compact".to_owned())),
        ]
    );
    assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
}

#[test]
fn an_empty_attribute_list_has_nothing_and_no_problems() {
    let parsed = parse("  ");
    assert_eq!(parsed, FenceAttributes::default());
}

#[test]
fn each_problem_is_named_and_its_attribute_left_out() {
    let cases = [
        ("shade=red", "shade"),
        ("cols=three", "cols"),
        ("cols=-1", "cols"),
        ("cols=1234567", "cols"),
        ("open=maybe", "open"),
        ("display=floating", "display"),
        ("variant=\"two words\"", "variant"),
        ("id=\"has space\"", "id"),
        ("title=", "title"),
        ("cols=2 cols=3", "cols"),
    ];
    for (raw, named) in cases {
        let parsed = parse(raw);
        assert_eq!(parsed.problems.len(), 1, "{raw}: {:?}", parsed.problems);
        assert!(
            parsed.problems.iter().any(|problem| problem.contains(named)),
            "{raw}: {:?}",
            parsed.problems
        );
        assert!(
            parsed.values.iter().all(|(key, _)| key != named) || raw.contains("cols=2 cols=3"),
            "{raw}"
        );
    }
}

#[test]
fn text_that_does_not_parse_as_attributes_is_one_problem_and_no_values() {
    for raw in [r#"title="unterminated"#, "=3", "cols=3 \"stray\"", "Title=x"] {
        let parsed = parse(raw);
        assert!(parsed.values.is_empty(), "{raw}");
        assert_eq!(parsed.problems.len(), 1, "{raw}: {:?}", parsed.problems);
    }
}
