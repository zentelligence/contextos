//! The typed fields a `workflow.toml` declares as `[[input]]` and `[[output]]`.

use std::fmt::Write as _;

use crate::package::{FieldKind, PackageError, TypedField};
use crate::workflow_definition::{WorkflowDefinition, WorkflowText};

fn definition(body: &str) -> Result<WorkflowDefinition, PackageError> {
    WorkflowDefinition::try_from(WorkflowText {
        text: &format!("name = \"Sync\"\nentry = \"sync.rhai\"\n{body}"),
        folder: "sync",
    })
}

fn reason(body: &str) -> String {
    match definition(body) {
        Err(error) => error.to_string(),
        Ok(parsed) => format!("accepted: {parsed:?}"),
    }
}

fn output_kinds(body: &str) -> Result<Vec<FieldKind>, PackageError> {
    Ok(definition(body)?
        .outputs
        .fields()
        .iter()
        .map(|field| field.kind.clone())
        .collect())
}

fn field(name: &str, kind: FieldKind) -> TypedField {
    TypedField {
        name: name.to_owned(),
        kind,
        required: true,
    }
}

#[test]
fn typed_output_fields_parse_every_documented_scalar_and_temporal_kind() -> Result<(), PackageError> {
    let kinds = [
        "number",
        "bool",
        "string",
        "null",
        "date",
        "time",
        "datetime",
        "timestamp",
    ];
    let body = kinds.iter().fold(String::new(), |mut text, kind| {
        let _ = write!(text, "[[output]]\nname = \"{kind}_field\"\nkind = \"{kind}\"\n");
        text
    });
    assert_eq!(
        output_kinds(&body)?,
        vec![
            FieldKind::Number,
            FieldKind::Bool,
            FieldKind::String,
            FieldKind::Null,
            FieldKind::Date,
            FieldKind::Time,
            FieldKind::DateTime,
            FieldKind::Timestamp
        ]
    );
    Ok(())
}

#[test]
fn an_array_output_declares_its_own_element_kind() -> Result<(), PackageError> {
    let kinds = output_kinds("[[output]]\nname = \"labels\"\nkind = \"array\"\n[output.items]\nkind = \"string\"\n")?;
    assert_eq!(
        kinds,
        vec![FieldKind::Array {
            items: Box::new(FieldKind::String)
        }]
    );
    Ok(())
}

#[test]
fn an_object_output_declares_its_own_properties() -> Result<(), PackageError> {
    let kinds = output_kinds(
        "[[output]]\nname = \"contact\"\nkind = \"object\"\n[[output.properties]]\nname = \"email\"\nkind = \"string\"\n[[output.properties]]\nname = \"age\"\nkind = \"number\"\n",
    )?;
    assert_eq!(
        kinds,
        vec![FieldKind::Object {
            properties: vec![field("email", FieldKind::String), field("age", FieldKind::Number)]
        }]
    );
    Ok(())
}

#[test]
fn an_array_of_objects_nests_two_levels_deep() -> Result<(), PackageError> {
    let kinds = output_kinds(
        "[[output]]\nname = \"contacts\"\nkind = \"array\"\n[output.items]\nkind = \"object\"\n[[output.items.properties]]\nname = \"email\"\nkind = \"string\"\n",
    )?;
    assert_eq!(
        kinds,
        vec![FieldKind::Array {
            items: Box::new(FieldKind::Object {
                properties: vec![field("email", FieldKind::String)]
            })
        }]
    );
    Ok(())
}

#[test]
fn malformed_fields_are_each_refused_naming_the_problem() {
    let cases = [
        ("[[output]]\nname = \"rows\"\nkind = \"array\"\n", "items"),
        (
            "[[output]]\nname = \"count\"\nkind = \"number\"\n[output.items]\nkind = \"string\"\n",
            "items",
        ),
        ("[[output]]\nname = \"\"\nkind = \"string\"\n", "no name"),
        ("[[output]]\nname = \"count\"\nkind = \"int64\"\n", "int64"),
        (
            "[[input]]\nname = \"a\"\nkind = \"string\"\n[[input]]\nname = \"a\"\nkind = \"string\"\n",
            "more than once",
        ),
    ];
    for (body, expected) in cases {
        let found = reason(body);
        assert!(found.contains(expected), "{body:?} gave {found}");
    }
}

#[test]
fn an_entry_path_that_could_leave_the_workflow_folder_is_refused() {
    for entry in [
        "/home/operator/sync.rhai",
        "../../etc/passwd",
        "..\\\\sync.rhai",
        "./sync.rhai",
    ] {
        let result = WorkflowDefinition::try_from(WorkflowText {
            text: &format!("name = \"Sync\"\nentry = \"{entry}\"\n"),
            folder: "sync",
        });
        let found = result
            .err()
            .map_or_else(|| "accepted".to_owned(), |error| error.to_string());
        assert!(found.contains("must be a plain relative path"), "{entry}: {found}");
    }
}

#[test]
fn a_workflow_may_set_its_own_log_level_and_an_unknown_one_is_refused() -> Result<(), PackageError> {
    assert_eq!(
        definition("log_level = \"debug\"\n")?.log_level,
        Some(crate::logging::LogLevel::Debug)
    );
    assert_eq!(definition("")?.log_level, None);
    assert!(reason("log_level = \"loud\"\n").contains("loud"));
    Ok(())
}

#[test]
fn a_workflow_may_declare_a_vault_input_and_says_where_it_is() -> Result<(), PackageError> {
    let parsed = definition("[[input]]\nname = \"target\"\nkind = \"array\"\n[input.items]\nkind = \"vault\"\n")?;
    assert_eq!(parsed.inputs.first_vault_input().as_deref(), Some("target[]"));
    assert_eq!(definition("")?.inputs.first_vault_input(), None);
    Ok(())
}
