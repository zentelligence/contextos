use super::*;
use crate::fake_vault::FakeVault;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PATH: &str = "vault://registry/workflows/crm/workflows.toml";

async fn read_with(vault: FakeVault) -> Result<Result<SourceRead, McpCallError>, Box<dyn std::error::Error>> {
    let client = vault.session().await?;
    Ok(read_source(&client, PATH).await)
}

#[tokio::test]
async fn a_file_that_is_there_is_its_text() -> TestResult {
    let vault = FakeVault::new().with_file("registry/workflows/crm/workflows.toml", "workflows = []\n");

    assert_eq!(
        read_with(vault).await?.map_err(|error| error.to_string())?,
        SourceRead::Text("workflows = []\n".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn a_file_that_is_not_there_is_missing() -> TestResult {
    let read = read_with(FakeVault::new().with_file("registry/other.md", "x")).await?;

    assert_eq!(read.map_err(|error| error.to_string())?, SourceRead::Missing);
    Ok(())
}

#[tokio::test]
async fn a_refusal_about_the_file_itself_is_reported_as_what_it_is() -> TestResult {
    for (code, reason) in [
        ("io/too-large", "is larger than the server will read"),
        (
            "path/outside-root",
            "is a link out of the vault, which the server does not follow",
        ),
        (
            "path/symlink-escape",
            "is a link out of the vault, which the server does not follow",
        ),
        ("io/binary", "is not text"),
        ("io/filesystem", "could not be read"),
    ] {
        let vault = FakeVault::new()
            .with_file("registry/workflows/crm/workflows.toml", "x")
            .refusing("registry/workflows/crm/workflows.toml", code);

        assert_eq!(
            read_with(vault).await?.map_err(|error| error.to_string())?,
            SourceRead::Unusable(reason),
            "{code}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_refusal_that_says_nothing_about_the_file_is_an_outage() -> TestResult {
    for code in [
        "io/vault-busy",
        "path/unknown-vault-name",
        "server/configuration",
        "tool/unavailable",
    ] {
        let vault = FakeVault::new()
            .with_file("registry/workflows/crm/workflows.toml", "x")
            .refusing("registry/workflows/crm/workflows.toml", code);

        assert!(
            matches!(read_with(vault).await?, Err(McpCallError::Refused { .. })),
            "{code} was taken for a defect in the file"
        );
    }
    Ok(())
}
