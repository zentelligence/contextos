//! `POST /{{vault_name}}/insights/` - the "New" note action (root only: a note's own
//! shard is derived from its title's slug, never chosen by the request path). Kept apart from
//! `routes::insights` (already close to this workspace's own file-size guidance) rather than
//! growing that file further, matching `routes::wiki_create`'s identical precedent of splitting a
//! screen's own creation action into its own module.
//!
//! The dedicated `GET .../new` form itself - and every rendering shared with it - stays in
//! `routes::insights`, since the one view every Insights response renders through lives there;
//! this module only ever calls back into it (`render_new_form_fragment`, `render_created_note`),
//! never the reverse.

use axum::extract::{Path, State};
use axum::http::HeaderValue;
use axum::response::Response;
use serde::Deserialize;

use crate::insights::{self, CreateInsightError};
use crate::insights_config;
use crate::routes::vault::html_response;

use super::insights::{InsightsRoutesState, ensure_vault, render_created_note, render_new_form_fragment};

#[derive(Debug, Default, Deserialize)]
struct CreateBody {
    /// The dedicated `/new` form's own submitted title - the only text field it carries, matching
    /// `routes::wiki_create::CreateBody`'s own corrected "no server-synthesised fallback" contract.
    /// Absent or blank/whitespace-only alike stay `CreateInsightError::EmptyTitle`; the form itself
    /// is always pre-filled with `insights::default_title`, so an operator only ever submits
    /// nothing here by clearing that field or via a malformed request, never through the header
    /// "New" button, which loads that form rather than posting directly
    /// (`routes::insights`'s own top-of-file doc).
    #[serde(default)]
    title: String,
    /// The form's own `status` `<select>`, always one of `insights::STATUSES` or blank - blank
    /// (the form's own default selection) stays `insights::create_insight`'s own domain-level
    /// `"proposed"` default, a genuine business rule rather than a routing-layer convenience, so
    /// unlike `title` this field keeps its existing defaulting behaviour unchanged.
    #[serde(default)]
    status: String,
}

fn create_error_message(error: &CreateInsightError) -> String {
    match error {
        CreateInsightError::EmptyTitle => "A note needs a title.".to_owned(),
        CreateInsightError::InvalidStatus => "Status must be one of proposed, adopted, or dismissed.".to_owned(),
        CreateInsightError::Write(_) => "The vault could not be reached.".to_owned(),
        CreateInsightError::Rejected => {
            "The note could not be created (a note with that slug may already exist).".to_owned()
        }
    }
}

/// `POST /{{vault_name}}/insights/`: creates a note titled
/// `title` with `status`, always through [`insights::create_insight`]
/// (`note_create`).
///
/// Every failure re-renders the `/new` form itself, never the plain shard-browse fragment: the
/// header "New" button always loads that dedicated form first (`routes::insights`'s own top-of-file
/// doc) rather than posting directly, so every real submission this handler ever sees already came
/// from it - matching `routes::wiki_create::create_dispatch`'s identical "any failure keeps the
/// operator on their own form" contract, the operator's own submitted title and status preserved
/// rather than lost.
pub async fn create(
    State(state): State<InsightsRoutesState>,
    Path(vault_name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Ok(parsed) = serde_json::from_slice::<CreateBody>(&body) else {
        return html_response("<p class=\"hint error\">Malformed request.</p>".to_owned());
    };
    let title = parsed.title;
    let config = insights_config::load_insights_config(&client, &vault_name).await;
    match insights::create_insight(&client, &vault_name, &config, &title, &parsed.status).await {
        Ok(relative_path) => {
            let target = crate::urls::VaultUrls::from(&*vault_name).insights(&relative_path);
            let fragment = Box::pin(render_created_note(&client, &vault_name, &config, &relative_path))
                .await
                .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            let mut response = html_response(fragment);
            if let Ok(value) = HeaderValue::from_str(&target) {
                response.headers_mut().insert("HX-Push-Url", value);
            }
            response
        }
        Err(error) => {
            let message = create_error_message(&error);
            let fragment = render_new_form_fragment(&vault_name, &config, title, &parsed.status, Some(message));
            html_response(fragment)
        }
    }
}
