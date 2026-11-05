use tempfile::tempdir;

use super::*;

fn bearer_record() -> CredentialRecord {
    CredentialRecord::Bearer {
        value: "sk-example-token".to_owned(),
        updated_at: "2026-09-20T00:00:00Z".to_owned(),
    }
}

fn client_secret_record() -> CredentialRecord {
    CredentialRecord::Oauth2ClientSecret {
        value: "client-secret-value".to_owned(),
        updated_at: "2026-09-20T00:00:00Z".to_owned(),
    }
}

fn token_record() -> CredentialRecord {
    CredentialRecord::Oauth2Token {
        access_token: "access-value".to_owned(),
        refresh_token: Some("refresh-value".to_owned()),
        expires_at: "2026-09-20T01:00:00Z".to_owned(),
        obtained_at: "2026-09-20T00:00:00Z".to_owned(),
    }
}

#[test]
fn round_trips_a_bearer_record_through_set_and_get() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));
    let record = bearer_record();

    store.set("some-bearer-server-token", &record)?;
    let fetched = store.get("some-bearer-server-token")?;

    assert_eq!(fetched, Some(record));
    Ok(())
}

#[test]
fn round_trips_an_oauth2_client_secret_record_through_set_and_get() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));
    let record = client_secret_record();

    store.set("some-oauth-server-client-secret", &record)?;
    let fetched = store.get("some-oauth-server-client-secret")?;

    assert_eq!(fetched, Some(record));
    Ok(())
}

#[test]
fn round_trips_an_oauth2_token_record_through_set_and_get() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));
    let record = token_record();

    store.set("some-oauth-server-token", &record)?;
    let fetched = store.get("some-oauth-server-token")?;

    assert_eq!(fetched, Some(record));
    Ok(())
}

#[test]
fn get_on_a_credential_ref_that_was_never_set_returns_none() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));

    let fetched = store.get("never-set-ref")?;

    assert_eq!(fetched, None);
    Ok(())
}

#[test]
fn delete_removes_a_previously_set_record() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));
    store.set("some-bearer-server-token", &bearer_record())?;

    store.delete("some-bearer-server-token")?;
    let fetched = store.get("some-bearer-server-token")?;

    assert_eq!(fetched, None);
    Ok(())
}

#[test]
fn delete_on_a_never_set_credential_ref_is_idempotent_ok() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));

    let result = store.delete("never-set-ref");

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn set_creates_the_root_directory_when_it_does_not_already_exist() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    assert!(!root.exists());
    let store = FsCredentialStore::new(root.clone());

    store.set("some-bearer-server-token", &bearer_record())?;

    assert!(root.is_dir());
    Ok(())
}

#[test]
fn rejects_a_credential_ref_containing_a_parent_directory_segment() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    let store = FsCredentialStore::new(root.clone());

    let result = store.set("../../etc/passwd", &bearer_record());

    assert!(matches!(result, Err(CredentialStoreError::InvalidCredentialRef { .. })));
    assert!(!root.exists(), "traversal attempt must never touch the filesystem");
    Ok(())
}

#[test]
fn rejects_a_credential_ref_containing_a_path_separator() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    let store = FsCredentialStore::new(root.clone());

    let result = store.set("some/server-token", &bearer_record());

    assert!(matches!(result, Err(CredentialStoreError::InvalidCredentialRef { .. })));
    assert!(!root.exists(), "path-separator ref must never touch the filesystem");
    Ok(())
}

#[test]
fn rejects_an_empty_credential_ref() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    let store = FsCredentialStore::new(root.clone());

    let result = store.set("", &bearer_record());

    assert!(matches!(result, Err(CredentialStoreError::InvalidCredentialRef { .. })));
    assert!(!root.exists());
    Ok(())
}

#[test]
fn rejects_an_absolute_path_shaped_credential_ref() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    let store = FsCredentialStore::new(root.clone());

    let result = store.get("/etc/passwd");

    assert!(matches!(result, Err(CredentialStoreError::InvalidCredentialRef { .. })));
    Ok(())
}

#[cfg(unix)]
#[test]
fn sets_owner_only_permissions_on_the_created_directory_and_written_file() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir()?;
    let root = dir.path().join("credentials");
    let store = FsCredentialStore::new(root.clone());

    store.set("some-bearer-server-token", &bearer_record())?;

    let dir_mode = std::fs::metadata(&root)?.permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "directory mode was {dir_mode:o}");

    let file_path = root.join("some-bearer-server-token.json");
    let file_mode = std::fs::metadata(&file_path)?.permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "file mode was {file_mode:o}");
    Ok(())
}
