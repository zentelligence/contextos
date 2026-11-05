//! What a job must take up when its successor names a release whose interface differs: the inputs and outputs a workflow
//! gained, lost, or changed between two releases, as the package ledger holds them.

use crate::package_release_manifest::{InterfaceField, WorkflowInterface};

#[cfg(test)]
#[path = "successor_interface_test.rs"]
mod tests;

/// Whether a change is to an input or an output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldSide {
    /// A declared input.
    Input,
    /// A declared output.
    Output,
}

impl FieldSide {
    /// The word the side is reported with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// What happened to one field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldChange {
    /// The release declares a field the earlier one did not.
    Added { required: bool },
    /// The earlier release declared a field this one does not.
    Removed,
    /// The field has another type.
    TypeChanged,
    /// The field was optional and a run must now supply it.
    BecameRequired,
    /// The field was required and a run may now leave it out.
    BecameOptional,
}

/// One change to a workflow's inputs or outputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceChange {
    /// Whether the field is an input or an output.
    pub side: FieldSide,
    /// The field's name.
    pub name: String,
    /// What happened to it.
    pub change: FieldChange,
}

impl std::fmt::Display for InterfaceChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = self.side.as_str();
        let name = &self.name;
        match self.change {
            FieldChange::Added { required: true } => write!(formatter, "{side} {name} added, and required"),
            FieldChange::Added { required: false } => write!(formatter, "{side} {name} added, and optional"),
            FieldChange::Removed => write!(formatter, "{side} {name} removed"),
            FieldChange::TypeChanged => write!(formatter, "{side} {name} has another type"),
            FieldChange::BecameRequired => write!(formatter, "{side} {name} is now required"),
            FieldChange::BecameOptional => write!(formatter, "{side} {name} is now optional"),
        }
    }
}

impl WorkflowInterface {
    /// The changes from `earlier` to this interface: inputs first, then outputs; within each, fields that were removed or
    /// changed in the order `earlier` declared them, then fields added in the order this one declares them.
    #[must_use]
    pub fn changes_since(&self, earlier: &Self) -> Vec<InterfaceChange> {
        let mut changes = fields_changed(FieldSide::Input, &earlier.inputs, &self.inputs);
        changes.extend(fields_changed(FieldSide::Output, &earlier.outputs, &self.outputs));
        changes
    }
}

fn fields_changed(side: FieldSide, earlier: &[InterfaceField], now: &[InterfaceField]) -> Vec<InterfaceChange> {
    let mut changes = Vec::new();
    for before in earlier {
        let change = match now.iter().find(|field| field.name == before.name) {
            None => Some(FieldChange::Removed),
            Some(field) if field.kind != before.kind => Some(FieldChange::TypeChanged),
            Some(field) if field.required && !before.required => Some(FieldChange::BecameRequired),
            Some(field) if !field.required && before.required => Some(FieldChange::BecameOptional),
            Some(_) => None,
        };
        changes.extend(change.map(|change| InterfaceChange {
            side,
            name: before.name.clone(),
            change,
        }));
    }
    changes.extend(
        now.iter()
            .filter(|field| !earlier.iter().any(|before| before.name == field.name))
            .map(|field| InterfaceChange {
                side,
                name: field.name.clone(),
                change: FieldChange::Added {
                    required: field.required,
                },
            }),
    );
    changes
}
