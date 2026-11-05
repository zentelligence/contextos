use super::*;

#[test]
fn the_registered_names_are_the_ones_already_installed_services_use() {
    assert_eq!(SERVICE_IDENTITY.program, "contextos-web");
    assert_eq!(SERVICE_IDENTITY.unit_name, "contextos-web.service");
    assert_eq!(SERVICE_IDENTITY.launchd_label, "au.zentelligence.contextos-web");
    assert_eq!(SERVICE_IDENTITY.windows_name, "ContextOS Web");
    assert_eq!(SERVICE_IDENTITY.config_file, "web.toml");
}

#[test]
fn the_process_record_stays_beside_web_toml_under_its_existing_name() {
    assert_eq!(
        contextos_service::process_record::path_for(std::path::Path::new("/home/op/web.toml")),
        std::path::PathBuf::from("/home/op/web.toml.process.json")
    );
}
