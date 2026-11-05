//! The typed inputs a vault-registered workflow declares in its `workflow.toml` `[[input]]` tables.
//!
//! The kinds are those of a package workflow. The `vault` kind is the exception: a vault-registered workflow's scope
//! is its own vault, so only a workflow from the host package store may declare one.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer};

use crate::package::{FieldKind, PackageError, TypedField, TypedFieldRaw, parse_typed_fields};

/// A workflow's declared inputs, with each name unique.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclaredInputs(Vec<TypedField>);

impl DeclaredInputs {
    /// The declared inputs in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[TypedField] {
        &self.0
    }

    /// The dotted path to the first input, at any depth, that has the `vault` kind, if any.
    #[must_use]
    pub fn first_vault_input(&self) -> Option<String> {
        self.0
            .iter()
            .find_map(|field| vault_kind_path(&field.name, &field.kind))
    }

    /// Keeps the declaration only when no input, at any depth, has the `vault` kind.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::VaultInputNotPermitted`] naming the first offending input.
    pub fn without_vault_kind(self) -> Result<Self, PackageError> {
        for field in &self.0 {
            if let Some(path) = vault_kind_path(&field.name, &field.kind) {
                return Err(PackageError::VaultInputNotPermitted { path });
            }
        }
        Ok(self)
    }
}

impl TryFrom<Vec<TypedFieldRaw>> for DeclaredInputs {
    type Error = PackageError;

    fn try_from(raw: Vec<TypedFieldRaw>) -> Result<Self, PackageError> {
        let fields = parse_typed_fields("input", raw)?;
        let mut seen = HashSet::new();
        for field in &fields {
            if !seen.insert(field.name.as_str()) {
                return Err(PackageError::DuplicateInput(field.name.clone()));
            }
        }
        Ok(Self(fields))
    }
}

impl<'de> Deserialize<'de> for DeclaredInputs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<TypedFieldRaw>::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

/// The dotted path to the first `vault`-kinded input within `kind`, if any.
pub(crate) fn vault_kind_path(path: &str, kind: &FieldKind) -> Option<String> {
    match kind {
        FieldKind::Vault => Some(path.to_owned()),
        FieldKind::Array { items } => vault_kind_path(&format!("{path}[]"), items),
        FieldKind::Object { properties } => properties
            .iter()
            .find_map(|property| vault_kind_path(&format!("{path}.{}", property.name), &property.kind)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "declared_inputs_test.rs"]
mod tests;
