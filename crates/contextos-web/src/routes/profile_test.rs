use super::*;

#[test]
fn identity_href_encodes_the_relative_path() {
    assert_eq!(
        identity_href("vault", "who am i.md"),
        "/vault/profile/identity/who%20am%20i.md"
    );
}

#[test]
fn entity_href_encodes_the_slug() {
    assert_eq!(entity_href("vault", "zenith"), "/vault/profile/entities/zenith");
}
