use super::*;

fn body(text: &str) -> Bytes {
    Bytes::from(text.to_owned())
}

#[test]
fn a_body_for_one_successor_needs_the_new_job_and_defaults_to_the_future_release() {
    let request = one_request(("ops", "sync", "nightly"), &body(r#"{"new_job":"nightly-next"}"#));
    assert!(matches!(
        request,
        Ok(SuccessorRequest {
            to: SuccessorTarget::Future,
            scope: SuccessorScope::One { .. },
            ..
        })
    ));
    assert!(one_request(("ops", "sync", "nightly"), &body("{}")).is_err());
}

#[test]
fn a_body_names_future_previous_or_an_exact_version_and_nothing_else() {
    for (to, expected) in [
        ("previous", SuccessorTarget::Previous),
        ("2.0.0", SuccessorTarget::Version(semver::Version::new(2, 0, 0))),
    ] {
        let request = all_request(("ops", "sync"), &body(&format!(r#"{{"to":"{to}"}}"#)));
        assert_eq!(request.map(|request| request.to).ok(), Some(expected));
    }
    assert!(all_request(("ops", "sync"), &body(r#"{"to":"^2"}"#)).is_err());
    assert!(all_request(("ops", "sync"), &body(r#"{"new_job":"x"}"#)).is_err());
}

#[test]
fn an_empty_body_for_every_successor_asks_for_the_defaults() {
    for text in ["", "  \n"] {
        let request = all_request(("ops", "sync"), &body(text));
        assert!(matches!(
            request,
            Ok(SuccessorRequest {
                to: SuccessorTarget::Future,
                scope: SuccessorScope::All,
                ..
            })
        ));
    }
}

#[test]
fn each_refusal_code_has_one_status() {
    let cases = [
        ("workflow/job-not-found", StatusCode::NOT_FOUND),
        ("workflow/package-unavailable", StatusCode::NOT_FOUND),
        ("workflow/job-file-invalid", StatusCode::UNPROCESSABLE_ENTITY),
        ("workflow/job-key-invalid", StatusCode::UNPROCESSABLE_ENTITY),
        ("workflow/transition-refused", StatusCode::CONFLICT),
        ("workflow/job-exists", StatusCode::CONFLICT),
        ("workflow/job-key-taken", StatusCode::CONFLICT),
        ("packages/store-locked", StatusCode::CONFLICT),
        ("workflow/job-store-unavailable", StatusCode::INTERNAL_SERVER_ERROR),
        ("workflow/state-unavailable", StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (code, status) in cases {
        assert_eq!(status_of(code), status, "{code}");
    }
}
