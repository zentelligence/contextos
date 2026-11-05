use super::*;

#[test]
fn a_recorded_validation_is_kept() {
    assert!(kept(Recording::Recorded).is_ok());
}

#[test]
fn a_validation_that_lost_a_race_for_the_state_is_a_conflict_to_repeat() {
    let result = kept(Recording::NotRecorded {
        conflict: true,
        reason: "the state changed".to_owned(),
    });

    assert!(matches!(result, Err(StoreError::Conflict)), "{result:?}");
}

#[test]
fn a_validation_that_could_not_be_written_says_why_and_is_not_a_conflict() -> Result<(), String> {
    let result = kept(Recording::NotRecorded {
        conflict: false,
        reason: "the server went away".to_owned(),
    });

    let Err(StoreError::Unavailable(message)) = result else {
        return Err(format!("expected unavailable, got {result:?}"));
    };
    assert!(
        message.contains("could not be recorded") && message.contains("the server went away"),
        "{message}"
    );
    Ok(())
}
