//! A workflow's declared outputs and the values its script sets with `set_output`.
//!
//! The declaration has the shape of a package workflow's `outputs`, and a vault workflow declares the same fields as
//! `[[output]]` tables. A value is validated against its declared kind at the moment the script sets it, so a script
//! can never hand a caller a value that does not match the declaration.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use rhai::{Dynamic, Engine, EvalAltResult};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::bridge::dynamic_to_json;
use crate::caller::WorkflowFailureKind;
use crate::declared_inputs::vault_kind_path;
use crate::package::{PackageError, TypedField, TypedFieldRaw, parse_typed_fields, validate_and_convert};
use crate::script::{LastCallFailure, throw};

/// The most one output value may hold once rendered as JSON, in bytes.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// A workflow's declared outputs, with each name unique.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclaredOutputs(Vec<TypedField>);

impl DeclaredOutputs {
    /// The declared outputs in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[TypedField] {
        &self.0
    }

    /// Keeps the declaration only when no output, at any depth, has the `vault` kind.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::VaultOutputNotPermitted`] naming the first offending output.
    pub fn without_vault_kind(self) -> Result<Self, PackageError> {
        for field in &self.0 {
            if let Some(path) = vault_kind_path(&field.name, &field.kind) {
                return Err(PackageError::VaultOutputNotPermitted { path });
            }
        }
        Ok(self)
    }
}

impl From<Vec<TypedField>> for DeclaredOutputs {
    fn from(fields: Vec<TypedField>) -> Self {
        Self(fields)
    }
}

impl TryFrom<Vec<TypedFieldRaw>> for DeclaredOutputs {
    type Error = PackageError;

    fn try_from(raw: Vec<TypedFieldRaw>) -> Result<Self, PackageError> {
        let fields = parse_typed_fields("output", raw)?;
        let mut seen = HashSet::new();
        for field in &fields {
            if !seen.insert(field.name.as_str()) {
                return Err(PackageError::DuplicateOutput(field.name.clone()));
            }
        }
        Ok(Self(fields))
    }
}

impl<'de> Deserialize<'de> for DeclaredOutputs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<TypedFieldRaw>::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

/// The values a script set, each in the normalised form of its declared kind, keyed by output name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutputValues(Map<String, Value>);

impl OutputValues {
    /// The values, keyed by output name.
    #[must_use]
    pub const fn values(&self) -> &Map<String, Value> {
        &self.0
    }

    /// The names that were set, in the order of their first being set.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

impl From<Map<String, Value>> for OutputValues {
    fn from(values: Map<String, Value>) -> Self {
        Self(values)
    }
}

/// Where `set_output` keeps what a script sets, shared between the registered function and the run.
pub(crate) type OutputStore = Arc<Mutex<Map<String, Value>>>;

/// The refusal a script sees when it sets an output badly, or a run ends on when a required one is missing.
fn refusal(message: &str) -> String {
    format!("{}: {message}", WorkflowFailureKind::OutputInvalid.code())
}

/// Registers `set_output(name, value)`: the value must be of the declared kind and fit [`MAX_OUTPUT_BYTES`], and the
/// last call for a name wins. A refusal is thrown to the script naming the output and the expected kind, never the
/// value, and is remembered so an uncaught refusal fails the run with the `output-invalid` kind.
pub(crate) fn register_set_output(
    engine: &mut Engine,
    declared: &DeclaredOutputs,
    store: &OutputStore,
    last_failure: &LastCallFailure,
) {
    let declared = declared.clone();
    let store = Arc::clone(store);
    let last_failure = Arc::clone(last_failure);
    engine.register_fn(
        "set_output",
        move |name: &str, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let refuse = |message: String| {
                Box::new(throw(
                    refusal(&message),
                    WorkflowFailureKind::OutputInvalid,
                    &last_failure,
                ))
            };
            let Some(field) = declared.fields().iter().find(|field| field.name == name) else {
                return Err(refuse(format!("output {name} is not declared by this workflow")));
            };
            let json = dynamic_to_json(&value).map_err(|_| {
                refuse(format!(
                    "output {name} has no JSON representation, so it cannot be a {}",
                    field.kind
                ))
            })?;
            let size = serde_json::to_vec(&json).map_or(usize::MAX, |bytes| bytes.len());
            if size > MAX_OUTPUT_BYTES {
                return Err(refuse(format!("output {name} is larger than {MAX_OUTPUT_BYTES} bytes")));
            }
            let converted = validate_and_convert(&json, &field.kind)
                .map_err(|_| refuse(format!("output {name} must be a {}", field.kind)))?;
            store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.to_owned(), converted);
            Ok(())
        },
    );
}

impl DeclaredOutputs {
    /// `set` with its entries in the order they are declared, so a reader sees them as the workflow lists them.
    pub(crate) fn in_declaration_order(&self, mut set: Map<String, Value>) -> Map<String, Value> {
        let mut ordered = Map::new();
        for field in &self.0 {
            if let Some(value) = set.remove(&field.name) {
                ordered.insert(field.name.clone(), value);
            }
        }
        ordered
    }

    /// The refusal text for a finished script that left required outputs unset, or `None` when every required
    /// output was set.
    pub(crate) fn missing_required(&self, set: &Map<String, Value>) -> Option<String> {
        let missing: Vec<&str> = self
            .0
            .iter()
            .filter(|field| field.required && !set.contains_key(&field.name))
            .map(|field| field.name.as_str())
            .collect();
        (!missing.is_empty()).then(|| refusal(&format!("required outputs were not set: {}", missing.join(", "))))
    }
}

#[cfg(test)]
#[path = "outputs_test.rs"]
mod tests;
