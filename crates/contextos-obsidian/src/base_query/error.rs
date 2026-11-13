use thiserror::Error;

/// Typed `base_query` execution failures: view/column resolution, filter
/// evaluation, and the documented grammar boundary. Distinct from
/// [`crate::BaseError`], which governs the `.base` YAML document itself.
#[derive(Debug, Error)]
pub enum BaseQueryError {
    #[error("Base query view {name:?} was not found")]
    ViewNotFound { name: String },
    #[error("Base query view {view} has no order or properties to resolve display columns from")]
    NoColumns { view: String },
    #[error("Base query definition is invalid at {path}: {violation}")]
    MalformedDefinition { path: String, violation: String },
    /// An inline definition given the shape of a `.base` file, whose views it would never read.
    #[error(
        "Base query inline definition holds views, but an inline definition is one view: its filters, order, sort, limit, properties, and formulas sit at the top level"
    )]
    InlineViews,
    /// An inline definition holding a key outside the one-view shape, which would otherwise be ignored.
    #[error("Base query inline definition holds {key:?}, which an inline definition does not accept")]
    UnknownInlineKey { key: String },
    #[error("Base query filter at {path} is not supported: {expression}")]
    UnsupportedFilterExpression { path: String, expression: String },
    #[error(
        "Base query filter at {path} references formula.{name}, which base_query never evaluates in a filter or sort key"
    )]
    FormulaReference { path: String, name: String },
    #[error("Base query filter at {path} references unsupported file property file.{property}")]
    UnsupportedFileProperty { path: String, property: String },
}

impl BaseQueryError {
    /// Returns the stable machine-readable `base_query` error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ViewNotFound { .. } => "query/base-view-not-found",
            Self::NoColumns { .. } => "query/base-query-no-columns",
            Self::MalformedDefinition { .. } | Self::InlineViews | Self::UnknownInlineKey { .. } => {
                "query/base-query-malformed"
            }
            Self::UnsupportedFilterExpression { .. } => "query/base-query-unsupported-filter",
            Self::FormulaReference { .. } => "query/base-query-formula-reference",
            Self::UnsupportedFileProperty { .. } => "query/base-query-unsupported-file-property",
        }
    }

    /// Returns an actionable correction for the failed query.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::ViewNotFound { .. } => "Pass an existing view name, or omit view to use the first view.",
            Self::NoColumns { .. } => {
                "Add an order list to the view or definition, or properties to the base, before querying it."
            }
            Self::MalformedDefinition { .. } => "Correct the reported definition path and retry.",
            Self::InlineViews => {
                "Give filters, order, sort, limit, properties, and formulas at the top level of an inline definition, or pass path and view to run a view of a .base file."
            }
            Self::UnknownInlineKey { .. } => {
                "An inline definition accepts only filters, order, sort, limit, properties, and formulas; remove the reported key."
            }
            Self::UnsupportedFilterExpression { .. } => {
                "Rewrite the filter using ==, !=, .contains(), .containsAny(...), .containsAll(...), file.hasTag(), file.inFolder(), &&, ||, !, or (...) grouping, or remove it."
            }
            Self::FormulaReference { .. } => {
                "base_query does not evaluate formula.* in a filter or sort key; remove the reference or filter/sort on a property instead."
            }
            Self::UnsupportedFileProperty { .. } => {
                "base_query supports file.ext, file.name, file.basename, file.path, file.folder, file.size, file.ctime, file.mtime, file.tags, file.links, file.embeds, file.backlinks, and file.properties; use a frontmatter property for anything else."
            }
        }
    }
}
