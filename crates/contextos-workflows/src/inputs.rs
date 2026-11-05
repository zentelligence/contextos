//! A run's input values, validated against the workflow's declared inputs.
//!
//! A workflow is the parameterised script and a job supplies the values. [`ValidatedInputs`] is the only form in
//! which values reach a script, so a script can never see a value that was not checked against the declaration.

use serde_json::{Map, Value};

use crate::package::{FieldKind, PackageError, TypedField, validate_and_convert};

/// The values a run supplies, with the inputs the workflow declares to check them against.
pub struct SuppliedInputs<'a> {
    pub declared: &'a [TypedField],
    pub values: Map<String, Value>,
}

/// A run's input values after validation: each in its normalised form, with the vault names among them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidatedInputs {
    values: Map<String, Value>,
    vaults: Vec<String>,
}

impl ValidatedInputs {
    /// The validated values, keyed by input name. An optional input that was not supplied is absent.
    #[must_use]
    pub const fn values(&self) -> &Map<String, Value> {
        &self.values
    }

    /// Every vault name held by a `vault` input, in declaration order and, within an array, in element order.
    #[must_use]
    pub fn vault_names(&self) -> &[String] {
        &self.vaults
    }
}

/// Input values as given on a command line, one `name=value` argument per value, with the inputs the workflow
/// declares. A value is plain text for a `string` or `vault` input and JSON for every other kind, since only text
/// kinds are unambiguous without quoting.
pub struct CommandLineInputs<'a> {
    pub declared: &'a [TypedField],
    pub arguments: Vec<String>,
}

impl TryFrom<CommandLineInputs<'_>> for ValidatedInputs {
    type Error = PackageError;

    fn try_from(command_line: CommandLineInputs<'_>) -> Result<Self, PackageError> {
        let values = parse_arguments(command_line.declared, &command_line.arguments)?;
        Self::try_from(SuppliedInputs {
            declared: command_line.declared,
            values,
        })
    }
}

/// Reads `name=value` arguments into values: plain text for a `string` or `vault` input and JSON for every other kind.
/// Refuses a missing `=`, a repeated name, an undeclared name, and a non-text value that is not JSON; the error names
/// the input, or the argument's position, and never quotes the value.
fn parse_arguments(declared: &[TypedField], arguments: &[String]) -> Result<Map<String, Value>, PackageError> {
    let mut values = Map::new();
    for (index, argument) in arguments.iter().enumerate() {
        let (name, text) = argument
            .split_once('=')
            .ok_or(PackageError::MalformedInputArgument { position: index + 1 })?;
        if values.contains_key(name) {
            return Err(PackageError::DuplicateInput(name.to_owned()));
        }
        let field = declared
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| PackageError::UndeclaredInput(name.to_owned()))?;
        let is_text = matches!(field.kind, FieldKind::String | FieldKind::Vault);
        let value = if is_text {
            Value::String(text.to_owned())
        } else {
            serde_json::from_str(text).map_err(|source| PackageError::UnreadableInputValue {
                name: name.to_owned(),
                message: source.to_string(),
            })?
        };
        values.insert(name.to_owned(), value);
    }
    Ok(values)
}

/// A saved job's values with the `name=value` arguments given for one run on top of them. Each argument replaces the
/// job's value for its name whole, parsed as a `workflows run` argument is, and may supply an optional input the job
/// omits. Nothing can be removed.
pub struct InputOverrides<'a> {
    pub declared: &'a [TypedField],
    /// The values the job's own file holds.
    pub job_values: Map<String, Value>,
    pub arguments: Vec<String>,
}

/// The inputs a run uses once its overrides are applied and the whole is validated, with the names that were overridden.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OverriddenInputs {
    pub inputs: ValidatedInputs,
    /// The overridden names, in declaration order. Never their values.
    pub overridden: Vec<String>,
}

impl TryFrom<InputOverrides<'_>> for OverriddenInputs {
    type Error = PackageError;

    fn try_from(overrides: InputOverrides<'_>) -> Result<Self, PackageError> {
        let InputOverrides {
            declared,
            mut job_values,
            arguments,
        } = overrides;
        let given = parse_arguments(declared, &arguments)?;
        let overridden = declared
            .iter()
            .filter(|field| given.contains_key(&field.name))
            .map(|field| field.name.clone())
            .collect();
        job_values.extend(given);
        let inputs = ValidatedInputs::try_from(SuppliedInputs {
            declared,
            values: job_values,
        })?;
        Ok(Self { inputs, overridden })
    }
}

impl TryFrom<SuppliedInputs<'_>> for ValidatedInputs {
    type Error = PackageError;

    fn try_from(supplied: SuppliedInputs<'_>) -> Result<Self, PackageError> {
        if let Some(name) = supplied
            .values
            .keys()
            .find(|name| !supplied.declared.iter().any(|field| &field.name == *name))
        {
            return Err(PackageError::UndeclaredInput(name.clone()));
        }
        let mut values = Map::new();
        let mut vaults = Vec::new();
        for field in supplied.declared {
            match supplied.values.get(&field.name) {
                Some(value) => {
                    let normalised = validate_and_convert(value, &field.kind)?;
                    collect_vault_names(&normalised, &field.kind, &mut vaults);
                    values.insert(field.name.clone(), normalised);
                }
                None if field.required => return Err(PackageError::MissingInput(field.name.clone())),
                None => {}
            }
        }
        Ok(Self { values, vaults })
    }
}

/// Appends every vault name `value` holds, walking it by its declared `kind`.
fn collect_vault_names(value: &Value, kind: &FieldKind, names: &mut Vec<String>) {
    match (kind, value) {
        (FieldKind::Vault, Value::String(name)) => names.push(name.clone()),
        (FieldKind::Array { items }, Value::Array(elements)) => {
            for element in elements {
                collect_vault_names(element, items, names);
            }
        }
        (FieldKind::Object { properties }, Value::Object(map)) => {
            for property in properties {
                if let Some(member) = map.get(&property.name) {
                    collect_vault_names(member, &property.kind, names);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "inputs_test.rs"]
mod tests;
