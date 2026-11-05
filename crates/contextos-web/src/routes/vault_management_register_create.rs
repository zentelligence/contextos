//! Creating a record in the Decisions register: `POST` on the register's root validates the submitted fields, then
//! creates the record through `base_record_create`, which assigns the id from the type's prefix when the register's
//! sidecar declares one and names the file from the title when it does not. Split out of
//! `routes::vault_management_registers`, which renders every view this module falls back to.

use axum::body::Bytes;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::McpCallError;
use crate::register_records::{ValidationContext, text, validate_fields};
use crate::routes::vault::unreachable;
use crate::slug::slugify;
use crate::vault_management_config::RecordType;
use crate::vault_relative_path::VaultRelativePath;

use super::tasks_key_prefixes::declared_prefixes;
use super::vault_management::{ManagementContext, Outcome, malformed};
use super::vault_management_registers::{
    FrontmatterReadResult, NewForm, Refusal, Register, new_view, parse_form, paths_view, render, validation_sources,
};

#[derive(Debug, Deserialize)]
struct CreatedRecord {
    path: String,
}

/// The id series the register's sidecar (`<base stem>.md`) declares; empty for an unkeyed (filename-keyed) base.
async fn sidecar_prefixes(context: &ManagementContext<'_>, base: &VaultRelativePath) -> Result<Vec<String>, Outcome> {
    let Some(sidecar) = base.as_str().strip_suffix(".base").map(|stem| format!("{stem}.md")) else {
        return Ok(Vec::new());
    };
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{}://{sidecar}", context.vault_name)),
    );
    match context.client.call_tool("frontmatter_read".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => Ok(result
            .into_typed::<FrontmatterReadResult>()
            .map(|read| declared_prefixes(&read.frontmatter))
            .unwrap_or_default()),
        Ok(_) => Ok(Vec::new()),
        Err(_) => Err(Outcome::from(unreachable())),
    }
}

/// The `base_record_create` arguments for a new record of `kind`, or why it cannot be created.
async fn create_arguments(
    context: &ManagementContext<'_>,
    register: &Register<'_>,
    kind: RecordType,
    mut frontmatter: Map<String, Value>,
    body: &str,
) -> Result<Result<Map<String, Value>, String>, Outcome> {
    frontmatter.retain(|_, value| !(value.is_null() || value.as_array().is_some_and(Vec::is_empty)));
    frontmatter.insert("type".to_owned(), Value::String(kind.value().to_owned()));
    let title = text(&frontmatter, "title");
    let prefix = &register.settings.record_type(kind).prefix;
    let base = &register.settings.paths.base;
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{}://{base}", context.vault_name)),
    );
    args.insert(
        "folder".to_owned(),
        Value::String(format!("/{}", register.settings.paths.dir)),
    );
    let declared = sidecar_prefixes(context, base).await?;
    if declared.is_empty() {
        let slug = slugify(&title);
        if slug.is_empty() {
            return Ok(Err(
                "The title needs at least one letter or digit to name the record's file.".to_owned(),
            ));
        }
        args.insert("filename".to_owned(), Value::String(format!("{slug}.md")));
    } else if declared.iter().any(|declared| declared == prefix) {
        args.insert("prefix".to_owned(), Value::String(prefix.clone()));
    } else {
        return Ok(Err(format!(
            "The register's sidecar does not declare the id prefix {prefix}, so a new {kind} cannot be given an id. \
             Add {prefix} to its key_prefixes, or change the {kind} id prefix above.",
            kind = kind.value()
        )));
    }
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert(
        "content".to_owned(),
        Value::String(format!("# {title}\n\n{}\n", body.replace("\r\n", "\n").trim())),
    );
    Ok(Ok(args))
}

pub(super) async fn create_record(context: &ManagementContext<'_>, register: &Register<'_>, body: &Bytes) -> Outcome {
    let Some((mut form, new_body, _)) = parse_form(body) else {
        return malformed();
    };
    let Some(kind) = form
        .remove("type")
        .and_then(|value| value.as_str().and_then(|value| RecordType::try_from(value).ok()))
    else {
        return malformed();
    };
    let (decision_ids, entity_slugs) = match validation_sources(context, register).await {
        Ok(sources) => sources,
        Err(outcome) => return outcome,
    };
    let empty = Map::new();
    let validation = ValidationContext {
        current: &empty,
        decision_ids: &decision_ids,
        entity_slugs: &entity_slugs,
    };
    let error = match validate_fields(Some(kind), &form, &validation) {
        Err(error) => error,
        Ok(frontmatter) => match create_arguments(context, register, kind, frontmatter, &new_body).await {
            Err(outcome) => return outcome,
            Ok(Err(error)) => error,
            Ok(Ok(args)) => match context.client.call_tool("base_record_create".to_owned(), args).await {
                Ok(result) if result.is_error != Some(true) => {
                    let location = result
                        .into_typed::<CreatedRecord>()
                        .ok()
                        .and_then(|created| VaultRelativePath::try_from(created.path).ok())
                        .and_then(|path| {
                            path.strip_ancestor(&register.settings.paths.dir)
                                .and_then(|relative| relative.strip_suffix(".md"))
                                .map(str::to_owned)
                        })
                        .unwrap_or_default();
                    return Outcome::Wrote { location };
                }
                Ok(result) => {
                    let code = result.into_typed::<Refusal>().ok().and_then(|refusal| refusal.code);
                    format!(
                        "The {} could not be created{}.",
                        kind.value(),
                        code.map(|code| format!(" ({code})")).unwrap_or_default()
                    )
                }
                Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
                    return Outcome::from(unreachable());
                }
            },
        },
    };
    let submitted = NewForm {
        values: &form,
        body: new_body,
        error,
    };
    match new_view(context, register, kind, Some(submitted)).await {
        Ok(main) => Outcome::Show(render(context, register, main, paths_view(context))),
        Err(outcome) => outcome,
    }
}
