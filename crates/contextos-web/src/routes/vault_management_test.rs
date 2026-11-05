use super::*;

#[test]
fn a_category_root_href_ends_with_a_slash_and_a_location_is_percent_encoded() {
    assert_eq!(
        category_href("vault", ManagementCategory::Standards, ""),
        "/vault/settings/standards/"
    );
    assert_eq!(
        category_href("vault", ManagementCategory::Standards, "console-ui/07 HTMx Uplifts.md"),
        "/vault/settings/standards/console-ui/07%20HTMx%20Uplifts.md"
    );
    assert_eq!(
        category_href("vault", ManagementCategory::Registry, "/agents/"),
        "/vault/settings/registry/agents"
    );
}

#[test]
fn every_configuration_pane_has_a_distinct_name() {
    let names: std::collections::HashSet<_> = CONFIG_PANES.iter().map(|pane| pane.name).collect();
    assert_eq!(names.len(), CONFIG_PANES.len());
    assert!(
        !names.contains("health"),
        "health is rendered apart from the configuration group"
    );
}
