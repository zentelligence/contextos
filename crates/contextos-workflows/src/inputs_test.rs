use serde_json::json;

use super::*;
use crate::package::FieldKind;

fn field(name: &str, kind: FieldKind, required: bool) -> TypedField {
    TypedField {
        name: name.to_owned(),
        kind,
        required,
    }
}

fn supplied<'a>(declared: &'a [TypedField], values: &Value) -> SuppliedInputs<'a> {
    SuppliedInputs {
        declared,
        values: match values {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        },
    }
}

#[test]
fn supplied_values_are_validated_and_returned_in_their_normalised_form() -> Result<(), PackageError> {
    let declared = [field("since", FieldKind::DateTime, true)];

    let inputs = ValidatedInputs::try_from(supplied(&declared, &json!({ "since": "2026-09-23T10:00:00+00:00" })))?;

    assert_eq!(inputs.values().get("since"), Some(&json!("2026-09-23T10:00:00Z")));
    Ok(())
}

#[test]
fn an_input_the_workflow_does_not_declare_is_rejected() {
    let declared = [field("since", FieldKind::String, true)];

    let outcome = ValidatedInputs::try_from(supplied(&declared, &json!({ "since": "x", "extra": 1 })));

    assert!(
        matches!(&outcome, Err(PackageError::UndeclaredInput(name)) if name == "extra"),
        "got {outcome:?}"
    );
}

#[test]
fn a_required_input_that_is_missing_is_rejected() {
    let declared = [field("since", FieldKind::String, true)];

    let outcome = ValidatedInputs::try_from(supplied(&declared, &json!({})));

    assert!(
        matches!(&outcome, Err(PackageError::MissingInput(name)) if name == "since"),
        "got {outcome:?}"
    );
}

#[test]
fn an_optional_input_that_is_missing_is_absent_and_gets_no_default() -> Result<(), PackageError> {
    let declared = [field("note", FieldKind::String, false)];

    let inputs = ValidatedInputs::try_from(supplied(&declared, &json!({})))?;

    assert!(inputs.values().is_empty());
    Ok(())
}

#[test]
fn a_value_of_the_wrong_kind_is_rejected() {
    let declared = [field("count", FieldKind::Number, true)];

    let outcome = ValidatedInputs::try_from(supplied(&declared, &json!({ "count": "three" })));

    assert!(
        matches!(outcome, Err(PackageError::TypeMismatch { .. })),
        "got {outcome:?}"
    );
}

#[test]
fn a_vault_value_that_looks_like_a_path_is_rejected() {
    let declared = [field("source", FieldKind::Vault, true)];

    let outcome = ValidatedInputs::try_from(supplied(&declared, &json!({ "source": "notes/a" })));

    assert!(
        matches!(outcome, Err(PackageError::InvalidVaultName { .. })),
        "got {outcome:?}"
    );
}

#[test]
fn vault_names_are_collected_from_top_level_array_and_object_inputs() -> Result<(), PackageError> {
    let declared = [
        field("source", FieldKind::Vault, true),
        field(
            "targets",
            FieldKind::Array {
                items: Box::new(FieldKind::Vault),
            },
            true,
        ),
        field(
            "options",
            FieldKind::Object {
                properties: vec![
                    field("archive", FieldKind::Vault, true),
                    field("label", FieldKind::String, true),
                ],
            },
            true,
        ),
    ];
    let values = json!({
        "options": { "label": "x", "archive": "cold" },
        "targets": ["client-b", "client-c"],
        "source": "harbour",
    });

    let inputs = ValidatedInputs::try_from(supplied(&declared, &values))?;

    assert_eq!(inputs.vault_names(), ["harbour", "client-b", "client-c", "cold"]);
    Ok(())
}

#[test]
fn an_optional_vault_input_left_out_names_no_vault() -> Result<(), PackageError> {
    let declared = [field("source", FieldKind::Vault, false)];

    let inputs = ValidatedInputs::try_from(supplied(&declared, &json!({})))?;

    assert!(
        inputs.vault_names().is_empty(),
        "`inputs.vault_names()` should be empty, got {:?}",
        inputs.vault_names()
    );
    Ok(())
}

#[test]
fn an_optional_property_of_an_object_input_may_be_left_out() -> Result<(), PackageError> {
    let declared = [field(
        "options",
        FieldKind::Object {
            properties: vec![
                field("label", FieldKind::String, true),
                field("archive", FieldKind::Vault, false),
            ],
        },
        true,
    )];

    let inputs = ValidatedInputs::try_from(supplied(&declared, &json!({ "options": { "label": "x" } })))?;

    assert_eq!(inputs.values().get("options"), Some(&json!({ "label": "x" })));
    assert!(
        inputs.vault_names().is_empty(),
        "`inputs.vault_names()` should be empty, got {:?}",
        inputs.vault_names()
    );
    Ok(())
}

fn arguments(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}

#[test]
fn a_string_or_vault_argument_is_taken_as_plain_text_even_when_it_looks_like_json() -> Result<(), PackageError> {
    let declared = [
        field("label", FieldKind::String, true),
        field("target", FieldKind::Vault, true),
    ];

    let inputs = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["label=123", "target=client-a"]),
    })?;

    assert_eq!(inputs.values().get("label"), Some(&json!("123")));
    assert_eq!(inputs.vault_names(), ["client-a".to_owned()]);
    Ok(())
}

#[test]
fn every_other_kind_is_read_as_json() -> Result<(), PackageError> {
    let declared = [
        field("count", FieldKind::Number, true),
        field("dry", FieldKind::Bool, true),
        field(
            "tags",
            FieldKind::Array {
                items: Box::new(FieldKind::String),
            },
            true,
        ),
        field("since", FieldKind::Date, true),
    ];

    let inputs = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["count=3", "dry=true", "tags=[\"a\",\"b\"]", "since=\"2026-01-01\""]),
    })?;

    assert_eq!(inputs.values().get("count"), Some(&json!(3)));
    assert_eq!(inputs.values().get("dry"), Some(&json!(true)));
    assert_eq!(inputs.values().get("tags"), Some(&json!(["a", "b"])));
    assert_eq!(inputs.values().get("since"), Some(&json!("2026-01-01")));
    Ok(())
}

#[test]
fn the_value_may_contain_equals_signs() -> Result<(), PackageError> {
    let declared = [field("query", FieldKind::String, true)];

    let inputs = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["query=a=b"]),
    })?;

    assert_eq!(inputs.values().get("query"), Some(&json!("a=b")));
    Ok(())
}

#[test]
fn an_argument_without_an_equals_sign_is_refused() {
    let declared = [field("label", FieldKind::String, true)];

    let result = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["label"]),
    });

    let message = result.err().map(|error| error.to_string());
    assert!(
        message.as_deref().is_some_and(|text| text.contains("no `=`")),
        "the refusal says what is wrong with the argument: {message:?}"
    );
}

#[test]
fn a_non_text_value_that_is_not_json_is_refused_naming_the_input() {
    let declared = [field("count", FieldKind::Number, true)];

    let result = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["count=three"]),
    });

    assert!(matches!(result, Err(PackageError::UnreadableInputValue { ref name, .. }) if name == "count"));
}

#[test]
fn an_input_given_twice_or_not_declared_is_refused() {
    let declared = [field("label", FieldKind::String, true)];

    let twice = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["label=a", "label=b"]),
    });
    let undeclared = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: arguments(&["label=a", "other=b"]),
    });

    assert!(matches!(twice, Err(PackageError::DuplicateInput(_))));
    assert!(matches!(undeclared, Err(PackageError::UndeclaredInput(_))));
}

#[test]
fn a_missing_required_argument_is_refused_by_the_same_validation() {
    let declared = [field("label", FieldKind::String, true)];

    let result = ValidatedInputs::try_from(CommandLineInputs {
        declared: &declared,
        arguments: Vec::new(),
    });

    assert!(matches!(result, Err(PackageError::MissingInput(_))));
}

fn job_values(values: &Value) -> Map<String, Value> {
    match values {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

fn overrides<'a>(declared: &'a [TypedField], job: &Value, arguments: &[&str]) -> InputOverrides<'a> {
    InputOverrides {
        declared,
        job_values: job_values(job),
        arguments: arguments.iter().map(|argument| (*argument).to_owned()).collect(),
    }
}

#[test]
fn an_override_replaces_the_jobs_value_whole_and_is_validated_as_job_values_are() -> Result<(), PackageError> {
    let declared = [
        field("target", FieldKind::String, true),
        field("limit", FieldKind::Number, true),
        field(
            "tags",
            FieldKind::Array {
                items: Box::new(FieldKind::String),
            },
            true,
        ),
        field("since", FieldKind::DateTime, true),
    ];
    let job = json!({ "target": "a", "limit": 5, "tags": ["x", "y"], "since": "2026-09-23T10:00:00Z" });

    let merged = OverriddenInputs::try_from(overrides(
        &declared,
        &job,
        &["target=b", r#"tags=["z"]"#, r#"since="2026-10-01T00:00:00+00:00""#],
    ))?;

    assert_eq!(merged.inputs.values().get("target"), Some(&json!("b")));
    assert_eq!(
        merged.inputs.values().get("limit"),
        Some(&json!(5)),
        "a value not overridden is the job's own"
    );
    assert_eq!(
        merged.inputs.values().get("tags"),
        Some(&json!(["z"])),
        "an array is replaced, never merged"
    );
    assert_eq!(
        merged.inputs.values().get("since"),
        Some(&json!("2026-10-01T00:00:00Z"))
    );
    assert_eq!(
        merged.overridden,
        vec!["target".to_owned(), "tags".to_owned(), "since".to_owned()],
        "overridden names are listed in declaration order"
    );

    let invalid = OverriddenInputs::try_from(overrides(&declared, &job, &["limit=not-json"]));
    assert!(
        invalid.is_err(),
        "an override is validated like a job value: {invalid:?}"
    );
    let mismatched = OverriddenInputs::try_from(overrides(&declared, &job, &[r#"limit="five""#]));
    assert!(
        matches!(mismatched, Err(PackageError::TypeMismatch { .. })),
        "{mismatched:?}"
    );
    Ok(())
}

#[test]
fn an_override_may_supply_an_optional_input_the_job_omits() -> Result<(), PackageError> {
    let declared = [
        field("target", FieldKind::String, true),
        field("note", FieldKind::String, false),
    ];

    let merged = OverriddenInputs::try_from(overrides(&declared, &json!({ "target": "a" }), &["note=hello"]))?;

    assert_eq!(merged.inputs.values().get("note"), Some(&json!("hello")));
    assert_eq!(merged.inputs.values().get("target"), Some(&json!("a")));
    assert_eq!(merged.overridden, vec!["note".to_owned()]);
    Ok(())
}

#[test]
fn an_override_naming_an_undeclared_input_is_refused_naming_it() {
    let declared = [field("target", FieldKind::String, true)];

    let outcome = OverriddenInputs::try_from(overrides(&declared, &json!({ "target": "a" }), &["nope=1"]));

    assert!(
        matches!(&outcome, Err(PackageError::UndeclaredInput(name)) if name == "nope"),
        "got {outcome:?}"
    );
}

#[test]
fn an_override_given_twice_is_refused() {
    let declared = [field("target", FieldKind::String, true)];

    let outcome = OverriddenInputs::try_from(overrides(
        &declared,
        &json!({ "target": "a" }),
        &["target=b", "target=c"],
    ));

    assert!(
        matches!(&outcome, Err(PackageError::DuplicateInput(name)) if name == "target"),
        "got {outcome:?}"
    );
}

#[test]
fn an_override_without_an_equals_sign_is_refused() {
    let declared = [field("target", FieldKind::String, true)];

    let outcome = OverriddenInputs::try_from(overrides(&declared, &json!({ "target": "a" }), &["target"]));

    let message = outcome.err().map(|error| error.to_string());
    assert!(
        message.as_deref().is_some_and(|text| text.contains("no `=`")),
        "the refusal says what is wrong with the argument: {message:?}"
    );
}

#[test]
fn an_override_is_text_for_string_and_vault_inputs_and_json_otherwise_exactly_as_on_workflows_run() -> Result<(), String>
{
    let cases = [
        (FieldKind::String, "plain text"),
        (FieldKind::String, "123"),
        (FieldKind::Vault, "work"),
        (FieldKind::Number, "42"),
        (FieldKind::Bool, "true"),
        (FieldKind::Date, "\"2026-10-21\""),
        (
            FieldKind::Array {
                items: Box::new(FieldKind::String),
            },
            r#"["a","b"]"#,
        ),
    ];
    for (kind, text) in cases {
        let declared = [field("value", kind.clone(), true)];
        let argument = format!("value={text}");

        let on_command_line = ValidatedInputs::try_from(CommandLineInputs {
            declared: &declared,
            arguments: vec![argument.clone()],
        })
        .map_err(|error| format!("{kind:?} {text}: command line: {error}"))?;
        let as_override = OverriddenInputs::try_from(overrides(&declared, &json!({}), &[argument.as_str()]))
            .map_err(|error| format!("{kind:?} {text}: override: {error}"))?;

        assert_eq!(as_override.inputs, on_command_line, "{kind:?} {text}");
    }
    Ok(())
}

#[test]
fn an_overridden_vault_input_is_among_the_runs_vault_names() -> Result<(), PackageError> {
    let declared = [
        field("source", FieldKind::Vault, true),
        field(
            "others",
            FieldKind::Array {
                items: Box::new(FieldKind::Vault),
            },
            false,
        ),
    ];

    let merged = OverriddenInputs::try_from(overrides(
        &declared,
        &json!({ "source": "va" }),
        &["source=vb", r#"others=["vc","vd"]"#],
    ))?;

    assert_eq!(merged.inputs.vault_names(), ["vb", "vc", "vd"]);
    Ok(())
}

#[test]
fn an_override_refusal_never_quotes_the_value() -> Result<(), String> {
    let declared = [
        field("when", FieldKind::DateTime, true),
        field("source", FieldKind::Vault, true),
        field("limit", FieldKind::Number, true),
    ];
    let job = json!({ "when": "2026-09-23T10:00:00Z", "source": "va", "limit": 1 });
    for argument in [
        "when=secret-not-a-datetime",
        "source=secret/path",
        "limit=secret-not-json",
        r#"limit="secret-string""#,
    ] {
        match OverriddenInputs::try_from(overrides(&declared, &job, &[argument])) {
            Ok(accepted) => return Err(format!("{argument}: accepted as {accepted:?}")),
            Err(error) => assert!(
                !error.to_string().contains("secret"),
                "{argument}: the refusal quotes the value: {error}"
            ),
        }
    }
    Ok(())
}

#[test]
fn a_malformed_argument_is_named_by_its_position_and_never_echoed() {
    // A secret mistyped without its `=` is still a secret, and a refusal is logged and printed.
    let declared = [field("token", FieldKind::String, true)];
    let job = json!({ "token": "a" });
    for secret in ["token-sk-live-4242", "sk-live-4242"] {
        let command_line = ValidatedInputs::try_from(CommandLineInputs {
            declared: &declared,
            arguments: arguments(&["token=ok", secret]),
        });
        let override_run = OverriddenInputs::try_from(overrides(&declared, &job, &["token=ok", secret]));

        for message in [
            command_line.err().map(|error| error.to_string()),
            override_run.err().map(|error| error.to_string()),
        ] {
            let message = message.unwrap_or_default();
            assert!(!message.is_empty(), "{secret}: a malformed argument was accepted");
            assert!(!message.contains(secret), "the refusal echoes the argument: {message}");
            assert!(
                message.contains("position 2") && message.contains("no `=`"),
                "the refusal names the argument by its position: {message}"
            );
        }
    }
}
