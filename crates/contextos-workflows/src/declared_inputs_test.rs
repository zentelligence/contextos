use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default, rename = "input")]
    inputs: DeclaredInputs,
}

fn parse(source: &str) -> Result<Manifest, toml::de::Error> {
    toml::from_str(source)
}

#[test]
fn a_manifest_without_input_tables_declares_no_inputs() -> Result<(), toml::de::Error> {
    let manifest = parse("")?;

    assert!(
        manifest.inputs.fields().is_empty(),
        "`manifest.inputs.fields()` should be empty, got {:?}",
        manifest.inputs.fields()
    );
    Ok(())
}

#[test]
fn input_tables_become_typed_fields_that_default_to_required() -> Result<(), toml::de::Error> {
    let manifest = parse(
        r#"
        [[input]]
        name = "since"
        kind = "date"
        required = false

        [[input]]
        name = "label"
        kind = "string"
        "#,
    )?;

    let fields = manifest.inputs.fields();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name, "since");
    assert_eq!(fields[0].kind, FieldKind::Date);
    assert!(!fields[0].required);
    assert!(fields[1].required);
    Ok(())
}

#[test]
fn nested_array_and_object_shapes_are_parsed() -> Result<(), toml::de::Error> {
    let manifest = parse(
        r#"
        [[input]]
        name = "tags"
        kind = "array"
        items = { kind = "string" }
        "#,
    )?;

    assert_eq!(
        manifest.inputs.fields()[0].kind,
        FieldKind::Array {
            items: Box::new(FieldKind::String)
        }
    );
    Ok(())
}

#[test]
fn an_unknown_key_on_an_input_table_is_rejected() {
    let result = parse("[[input]]\nname = \"a\"\nkind = \"string\"\ndefault = \"x\"\n");

    assert!(result.is_err());
}

#[test]
fn an_unrecognised_kind_is_rejected_naming_the_input() {
    let message = parse("[[input]]\nname = \"a\"\nkind = \"blob\"\n")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(message.contains("blob"), "{message}");
}

#[test]
fn two_inputs_with_the_same_name_are_rejected() {
    let message = parse("[[input]]\nname = \"a\"\nkind = \"string\"\n[[input]]\nname = \"a\"\nkind = \"number\"\n")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(message.contains("declared more than once"), "{message}");
}

#[test]
fn a_vault_kind_is_refused_for_a_workflow_whose_scope_is_its_own_vault() -> Result<(), toml::de::Error> {
    let manifest = parse("[[input]]\nname = \"target\"\nkind = \"vault\"\n")?;

    let error = manifest.inputs.without_vault_kind().err();

    assert!(
        matches!(&error, Some(PackageError::VaultInputNotPermitted { path }) if path == "target"),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn a_vault_kind_nested_in_an_array_or_object_is_also_refused() -> Result<(), toml::de::Error> {
    let in_array = parse("[[input]]\nname = \"all\"\nkind = \"array\"\nitems = { kind = \"vault\" }\n")?;
    let in_object =
        parse("[[input]]\nname = \"pair\"\nkind = \"object\"\nproperties = [{ name = \"from\", kind = \"vault\" }]\n")?;

    assert!(in_array.inputs.without_vault_kind().is_err());
    assert!(in_object.inputs.without_vault_kind().is_err());
    Ok(())
}

#[test]
fn inputs_without_a_vault_kind_pass_the_vault_kind_check_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = parse("[[input]]\nname = \"since\"\nkind = \"date\"\n")?;

    let kept = manifest.inputs.without_vault_kind()?;

    assert_eq!(kept.fields().len(), 1);
    Ok(())
}
