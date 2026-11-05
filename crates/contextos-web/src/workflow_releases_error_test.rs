use super::*;

#[test]
fn each_failure_has_a_stable_code() {
    let name = WorkflowReleasesError::Name {
        what: "package",
        value: "Bad Name".to_owned(),
        reason: "not lower case".to_owned(),
    };
    let config = WorkflowReleasesError::ConfigInvalid {
        reason: "no file".to_owned(),
    };
    let not_connected = WorkflowReleasesError::NotConnected { reason: "no server" };
    let state = WorkflowReleasesError::State {
        reason: "bad toml".to_owned(),
    };
    let host = WorkflowReleasesError::Host {
        reason: "no data folder",
    };

    assert_eq!(name.code(), "packages/invalid-name");
    assert_eq!(config.code(), "workflow/config-invalid");
    assert_eq!(not_connected.code(), "workflow/server-unavailable");
    assert_eq!(state.code(), "workflow/state-unavailable");
    assert_eq!(host.code(), "workflow/state-unavailable");
}
