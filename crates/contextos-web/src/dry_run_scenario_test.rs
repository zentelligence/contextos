use serde_json::json;

use super::*;

fn scenario(source: &str) -> Result<Scenario, ScenarioError> {
    Scenario::parse(source)
}

#[test]
fn a_valid_call_declares_a_response_to_return() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "valid"
            response = { contacts = [] }
        "#,
    )?;
    assert_eq!(scenario.calls.len(), 1);
    assert_eq!(scenario.calls[0].server, "crm");
    assert_eq!(scenario.calls[0].tool, "list_contacts");
    assert_eq!(
        scenario.calls[0].response,
        ScenarioResponse::Valid(json!({"contacts": []}))
    );
    Ok(())
}

#[test]
fn an_invalid_call_declares_a_response_not_checked_against_the_output_schema() -> Result<(), Box<dyn std::error::Error>>
{
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "invalid"
            response = { unexpected = true }
        "#,
    )?;
    assert_eq!(
        scenario.calls[0].response,
        ScenarioResponse::Invalid(json!({"unexpected": true}))
    );
    Ok(())
}

#[test]
fn an_error_call_declares_a_message_instead_of_a_response() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "error"
            error = "rate limited"
        "#,
    )?;
    assert_eq!(
        scenario.calls[0].response,
        ScenarioResponse::Error("rate limited".to_owned())
    );
    Ok(())
}

#[test]
fn a_valid_call_with_no_response_is_malformed() {
    let result = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "valid"
        "#,
    );
    assert!(
        matches!(result, Err(ScenarioError::MissingResponse { .. })),
        "{result:?}"
    );
}

#[test]
fn an_error_call_with_no_error_message_is_malformed() {
    let result = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "error"
        "#,
    );
    assert!(matches!(result, Err(ScenarioError::MissingError { .. })), "{result:?}");
}

#[test]
fn an_error_call_that_also_declares_a_response_is_malformed() {
    let result = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "error"
            error = "rate limited"
            response = { contacts = [] }
        "#,
    );
    assert!(
        matches!(result, Err(ScenarioError::ConflictingResponseAndError { .. })),
        "{result:?}"
    );
}

#[test]
fn an_empty_scenario_is_malformed() {
    let result = scenario("");
    assert!(matches!(result, Err(ScenarioError::Empty)), "{result:?}");
}

#[test]
fn matching_consumes_calls_in_declared_order() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "valid"
            response = { contacts = [] }

            [[call]]
            server = "crm"
            tool = "update_contact"
            kind = "valid"
            response = { updated = true }
        "#,
    )?;
    let matcher = ScenarioMatcher::new(scenario);

    let first = matcher.next_call("crm", "list_contacts")?;
    assert_eq!(first, ScenarioResponse::Valid(json!({"contacts": []})));
    let second = matcher.next_call("crm", "update_contact")?;
    assert_eq!(second, ScenarioResponse::Valid(json!({"updated": true})));
    Ok(())
}

#[test]
fn a_call_out_of_the_declared_order_is_a_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "valid"
            response = { contacts = [] }

            [[call]]
            server = "crm"
            tool = "update_contact"
            kind = "valid"
            response = { updated = true }
        "#,
    )?;
    let matcher = ScenarioMatcher::new(scenario);

    let result = matcher.next_call("crm", "update_contact");

    assert!(
        matches!(result, Err(ScenarioError::Mismatch { position: 0, .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_call_past_the_end_of_the_scenario_is_exhausted() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = scenario(
        r#"
            [[call]]
            server = "crm"
            tool = "list_contacts"
            kind = "valid"
            response = { contacts = [] }
        "#,
    )?;
    let matcher = ScenarioMatcher::new(scenario);
    matcher.next_call("crm", "list_contacts")?;

    let result = matcher.next_call("crm", "list_contacts");

    assert!(
        matches!(result, Err(ScenarioError::Exhausted { position: 1 })),
        "{result:?}"
    );
    Ok(())
}
