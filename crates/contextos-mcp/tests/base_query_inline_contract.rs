//! `base_query` with an inline `definition`: the definition is one view at the top level, so the shape of a `.base`
//! file (a `views` list), any key outside that one-view shape, and a `view` name alongside it are refused rather than
//! ignored, since an ignored key leaves the caller believing it took effect.
mod support;

use serde_json::{Value, json};
use support::git_harness::{BoxError, Harness, harness_with};

async fn seeded() -> Result<Harness, BoxError> {
    harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join("events"))?;
            std::fs::write(
                root.join("events/one.md"),
                "---\nkind: run-finished\nrun_id: S-001\n---\n# One\n",
            )?;
            std::fs::write(
                root.join("events/two.md"),
                "---\nkind: run-started\nrun_id: S-002\n---\n# Two\n",
            )
        },
    )
    .await
}

fn refusal(result: &rmcp::model::CallToolResult) -> Result<&serde_json::Map<String, Value>, BoxError> {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    Ok(result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or("the refusal carried no structured content")?)
}

#[tokio::test]
async fn an_inline_definition_in_the_shape_of_a_base_file_is_refused_pointing_at_the_one_view_shape()
-> Result<(), BoxError> {
    let harness = seeded().await?;
    let result = harness
        .call(
            "base_query",
            json!({
                "definition": {
                    "filters": "file.inFolder(\"events\") && kind == \"run-finished\"",
                    "views": [{"type": "table", "name": "x", "order": ["file.name", "run_id"]}]
                },
                "format": "json"
            }),
        )
        .await?;

    let body = refusal(&result)?;
    assert_eq!(body.get("code"), Some(&json!("query/base-query-malformed")), "{body:?}");
    let remediation = body.get("remediation").and_then(Value::as_str).unwrap_or_default();
    assert!(
        remediation.contains("top level") && remediation.contains("path and view"),
        "{remediation}"
    );
    Ok(())
}

#[tokio::test]
async fn an_inline_definition_key_outside_the_one_view_shape_is_refused_naming_it() -> Result<(), BoxError> {
    let harness = seeded().await?;
    let result = harness
        .call(
            "base_query",
            json!({"definition": {"filters": "file.inFolder(\"events\")", "order": ["file.name"], "groupBy": "kind"}}),
        )
        .await?;

    let body = refusal(&result)?;
    assert_eq!(body.get("code"), Some(&json!("query/base-query-malformed")), "{body:?}");
    let message = body.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("groupBy"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_view_name_given_with_an_inline_definition_is_refused() -> Result<(), BoxError> {
    let harness = seeded().await?;
    let result = harness
        .call(
            "base_query",
            json!({"definition": {"filters": "file.inFolder(\"events\")", "order": ["file.name"]}, "view": "x"}),
        )
        .await?;

    let body = refusal(&result)?;
    assert_eq!(body.get("code"), Some(&json!("io/invalid-argument")), "{body:?}");
    let message = body.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("path"), "{message}");
    Ok(())
}

#[tokio::test]
async fn the_one_view_shape_queries_inline_with_its_order_as_the_columns() -> Result<(), BoxError> {
    let harness = seeded().await?;
    let body = harness
        .structured(
            "base_query",
            json!({
                "definition": {
                    "filters": "file.inFolder(\"events\") && kind == \"run-finished\"",
                    "order": ["file.name", "run_id"]
                },
                "format": "json"
            }),
        )
        .await?;

    assert_eq!(body.get("matched"), Some(&json!(1)), "{body}");
    assert_eq!(body.get("columns"), Some(&json!(["file.name", "run_id"])), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_advertised_definition_parameter_describes_the_one_view_shape() -> Result<(), BoxError> {
    let catalogue = contextos_mcp::ContextOsServer::catalogue().list_all();
    let tool = catalogue
        .iter()
        .find(|tool| tool.name == "base_query")
        .ok_or("base_query is not advertised")?;
    let description = tool
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("definition"))
        .and_then(|definition| definition.get("description"))
        .and_then(Value::as_str)
        .ok_or("the definition parameter has no description")?;

    for key in ["filters", "order", "sort", "limit", "properties", "formulas", "views"] {
        assert!(description.contains(&format!("`{key}`")), "{key}: {description}");
    }
    Ok(())
}
