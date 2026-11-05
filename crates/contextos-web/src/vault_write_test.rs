use super::*;

#[test]
fn a_refusal_reason_names_the_problem_without_echoing_server_detail() {
    assert_eq!(WriteOutcome::Written.refusal_reason(), None);
    let conflict = WriteOutcome::Refused {
        code: Some(CONFLICT.to_owned()),
    };
    assert!(
        conflict
            .refusal_reason()
            .is_some_and(|reason| reason.contains("changed after you opened it"))
    );
    let exists = WriteOutcome::Refused {
        code: Some(ALREADY_EXISTS.to_owned()),
    };
    assert!(
        exists
            .refusal_reason()
            .is_some_and(|reason| reason.contains("already exists"))
    );
    let other = WriteOutcome::Refused {
        code: Some("path/outside-root".to_owned()),
    };
    assert_eq!(
        other.refusal_reason().as_deref(),
        Some("The vault refused this change (path/outside-root).")
    );
}
