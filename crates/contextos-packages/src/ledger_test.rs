use super::*;

#[test]
fn a_recorded_cause_is_reported_as_the_same_removal_cause() {
    assert_eq!(RemovalCause::from(RecordedCause::Retention), RemovalCause::Retention);
    assert_eq!(RemovalCause::from(RecordedCause::Forced), RemovalCause::Forced);
    assert_eq!(RemovalCause::from(RecordedCause::ByRequest), RemovalCause::ByRequest);
}

#[test]
fn a_requested_removal_is_recorded_as_the_matching_cause() {
    assert_eq!(
        RecordedCause::from(RequestedRemoval::ByRequest),
        RecordedCause::ByRequest
    );
    assert_eq!(RecordedCause::from(RequestedRemoval::Forced), RecordedCause::Forced);
}
