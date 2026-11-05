use std::path::Path;

use super::*;

const HOME: &str = "/Users/pj";
const LOCAL_DATA: &str = "D:/Redirected/Local";

fn offered(os: &str, file_name: &str) -> Option<std::path::PathBuf> {
    offered_log_file_on(os, Some(Path::new(HOME)), Some(Path::new(LOCAL_DATA)), file_name)
}

#[test]
fn windows_is_offered_the_named_file_under_the_local_data_folder_wherever_it_has_been_moved() {
    assert_eq!(
        offered("windows", "contextos.log"),
        Some(
            Path::new(LOCAL_DATA)
                .join("contextos")
                .join("logs")
                .join("contextos.log")
        )
    );
}

#[test]
fn macos_is_offered_the_named_file_under_the_user_logs_folder() {
    assert_eq!(
        offered("macos", "contextos.log"),
        Some(
            Path::new(HOME)
                .join("Library")
                .join("Logs")
                .join("contextos")
                .join("contextos.log")
        )
    );
}

#[test]
fn linux_whose_service_manager_keeps_standard_error_in_its_journal_is_offered_none() {
    assert_eq!(offered("linux", "contextos.log"), None);
}

#[test]
fn each_binary_is_offered_its_own_file_in_the_same_folder() {
    for os in ["windows", "macos"] {
        let server = offered(os, "contextos.log");
        let web = offered(os, "contextos-web.log");

        assert_ne!(server, web, "{os}");
        assert_eq!(
            server.as_deref().and_then(Path::parent),
            web.as_deref().and_then(Path::parent),
            "{os}"
        );
    }
}

#[test]
fn a_host_whose_folder_cannot_be_found_is_offered_none() {
    assert_eq!(
        offered_log_file_on("windows", Some(Path::new(HOME)), None, "contextos.log"),
        None
    );
    assert_eq!(
        offered_log_file_on("macos", None, Some(Path::new(LOCAL_DATA)), "contextos.log"),
        None
    );
}

#[test]
fn the_offered_rotation_is_ten_megabytes_kept_for_seven_days() {
    assert_eq!(OFFERED_MAX_SIZE_MB, 10);
    assert_eq!(OFFERED_RETENTION_DAYS, 7);
}
