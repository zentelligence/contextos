use super::*;
use crate::test_sources::{MemoryLibraries as Memory, manifest_naming, manifest_requiring};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn names(raw: &[&str]) -> Result<Vec<LibraryName>, Box<dyn std::error::Error>> {
    Ok(raw
        .iter()
        .map(|name| LibraryName::try_from(*name))
        .collect::<Result<_, _>>()?)
}

async fn read(memory: &Memory, named: &[&str]) -> Result<VaultLibraries, LibraryError> {
    let named = names(named).map_err(|error| LibraryError::Unreachable(error.to_string()))?;
    read_libraries(&named, memory, &mut SnapshotBudget::default()).await
}

#[tokio::test]
async fn a_named_library_and_the_libraries_it_names_are_read_in_name_order() -> TestResult {
    let memory = Memory::new()
        .simple("crm", &["http-helpers"])
        .simple("http-helpers", &[])
        .simple("unrelated", &[]);

    let libraries = read(&memory, &["crm"]).await?;

    let reached: Vec<&str> = libraries.iter().map(|library| library.name().as_str()).collect();
    assert_eq!(reached, ["crm", "http-helpers"]);
    let crm = libraries.iter().next().ok_or("crm missing")?;
    assert_eq!(crm.modules().len(), 1);
    assert_eq!(crm.modules()[0].text(), "fn id() { 1 }");
    assert!(
        memory.read_log().iter().all(|read| !read.starts_with("unrelated/")),
        "a library nothing names was read: {:?}",
        memory.read_log()
    );
    Ok(())
}

#[tokio::test]
async fn a_library_that_states_a_requirement_on_another_does_not_read_that_librarys_source() -> TestResult {
    let memory = Memory::new()
        .library(
            "crm",
            &manifest_requiring(&["http-helpers"]),
            &[("client.rhai", "fn id() { 1 }")],
        )
        .simple("http-helpers", &[]);

    let libraries = read(&memory, &["crm"]).await?;

    let reached: Vec<&str> = libraries.iter().map(|library| library.name().as_str()).collect();
    assert_eq!(reached, ["crm"]);
    assert!(
        memory.read_log().iter().all(|read| !read.starts_with("http-helpers/")),
        "a library with a stated requirement was read from source: {:?}",
        memory.read_log()
    );
    Ok(())
}

#[tokio::test]
async fn a_library_reached_twice_is_read_once() -> TestResult {
    let memory = Memory::new()
        .simple("a", &["shared"])
        .simple("b", &["shared"])
        .simple("shared", &[]);

    let libraries = read(&memory, &["a", "b"]).await?;

    assert_eq!(libraries.iter().count(), 3);
    let shared_manifest_reads = memory
        .read_log()
        .iter()
        .filter(|read| read.as_str() == "shared/library.toml")
        .count();
    assert_eq!(shared_manifest_reads, 1);
    Ok(())
}

#[tokio::test]
async fn a_library_with_no_folder_is_not_found_and_named() -> TestResult {
    let memory = Memory::new().simple("crm", &[]);

    let error = read(&memory, &["other"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::NotFound { library, .. } if library.as_str() == "other"),
        "{error:?}"
    );
    assert!(error.to_string().contains("other"), "{error}");
    Ok(())
}

#[tokio::test]
async fn a_library_a_workflow_does_not_name_is_not_found_even_when_a_library_names_it_and_is_missing() -> TestResult {
    let memory = Memory::new().simple("crm", &["gone"]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::NotFound { library, .. } if library.as_str() == "gone"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_folder_that_differs_only_in_case_is_not_taken_for_the_library() -> TestResult {
    let memory = Memory::new().simple("CRM", &[]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(matches!(&error, LibraryError::NotFound { .. }), "{error:?}");
    assert!(
        error.to_string().contains("CRM"),
        "the case-different folder is named: {error}"
    );
    assert!(
        memory.read_log().is_empty(),
        "a folder that is not the library was read"
    );
    Ok(())
}

#[tokio::test]
async fn two_folders_that_differ_only_in_case_are_reported_and_neither_is_read() -> TestResult {
    let memory = Memory::new().simple("crm", &[]).simple("CRM", &[]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Ambiguous { folders, .. } if folders.len() == 2),
        "{error:?}"
    );
    assert_eq!(memory.read_log().len(), 0);
    Ok(())
}

#[tokio::test]
async fn libraries_that_name_each_other_are_a_cycle_naming_the_loop() -> TestResult {
    let memory = Memory::new().simple("a", &["b"]).simple("b", &["a"]);

    let error = read(&memory, &["a"]).await.err().ok_or("nothing failed")?;

    let LibraryError::Cycle { path } = &error else {
        return Err(format!("expected a cycle, got {error:?}").into());
    };
    let path: Vec<&str> = path.iter().map(LibraryName::as_str).collect();
    assert_eq!(path, ["a", "b", "a"]);
    Ok(())
}

#[tokio::test]
async fn a_library_that_names_itself_is_a_cycle() -> TestResult {
    let memory = Memory::new().simple("a", &["a"]);

    let error = read(&memory, &["a"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Cycle { path } if path.len() == 2),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_library_toml_that_breaks_its_schema_is_reported_against_that_library() -> TestResult {
    let memory = Memory::new().library("crm", "modules = []\nexports = []\n", &[]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Manifest { library, .. } if library.as_str() == "crm"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_declared_module_that_cannot_be_read_names_the_library_and_the_file() -> TestResult {
    let memory = Memory::new().library("crm", &manifest_naming(&[]), &[]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Unreadable { library, path, .. } if library.as_str() == "crm" && path == "client.rhai"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_over_the_per_file_limit_is_refused_from_its_size_without_being_read() -> TestResult {
    let big = "x".repeat(usize::try_from(crate::workflow_files::MAX_WORKFLOW_FILE_BYTES)? + 1);
    let memory = Memory::new().library("crm", &manifest_naming(&[]), &[("client.rhai", &big)]);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Unreadable { path, .. } if path == "client.rhai"),
        "{error:?}"
    );
    assert!(
        !memory.read_log().contains(&"crm/client.rhai".to_owned()),
        "the oversized file was read"
    );
    Ok(())
}

#[tokio::test]
async fn a_manifest_that_grew_past_the_per_file_limit_after_its_size_was_checked_is_refused_as_unreadable() -> TestResult
{
    let padding = "x".repeat(usize::try_from(crate::workflow_files::MAX_WORKFLOW_FILE_BYTES)? + 1);
    let grown = format!("{}# {padding}\n", manifest_naming(&[]));
    let memory = Memory::new()
        .library("crm", &grown, &[("client.rhai", "fn id() { 1 }")])
        .reporting_size("crm", "library.toml", 10);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Unreadable { path, message, .. } if path == "library.toml" && message.contains("larger")),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_module_that_grew_past_the_per_file_limit_after_its_size_was_checked_is_refused_as_unreadable() -> TestResult
{
    let big = "x".repeat(usize::try_from(crate::workflow_files::MAX_WORKFLOW_FILE_BYTES)? + 1);
    let memory = Memory::new()
        .library("crm", &manifest_naming(&[]), &[("client.rhai", &big)])
        .reporting_size("crm", "client.rhai", 10);

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(
        matches!(&error, LibraryError::Unreadable { path, .. } if path == "client.rhai"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn thirty_two_libraries_are_read_and_a_thirty_third_is_refused() -> TestResult {
    let build = |count: usize| {
        let mut memory = Memory::new();
        let mut roots = Vec::new();
        for index in 0..count {
            let name = format!("lib-{index:02}");
            memory = memory.simple(&name, &[]);
            roots.push(name);
        }
        (memory, roots)
    };

    let (memory, roots) = build(32);
    let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
    assert_eq!(read(&memory, &roots).await?.iter().count(), 32);

    let (memory, roots) = build(33);
    let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
    let error = read(&memory, &roots).await.err().ok_or("nothing failed")?;
    assert!(matches!(&error, LibraryError::TooMany { limit: 32, .. }), "{error:?}");
    assert!(error.to_string().contains("32"), "{error}");
    Ok(())
}

#[tokio::test]
async fn a_chain_eight_levels_deep_is_read_and_a_ninth_level_is_refused() -> TestResult {
    let chain = |depth: usize| {
        let mut memory = Memory::new();
        for index in 0..depth {
            let name = format!("level-{index}");
            let next = format!("level-{}", index + 1);
            let others: Vec<&str> = if index + 1 < depth { vec![next.as_str()] } else { vec![] };
            memory = memory.simple(&name, &others);
        }
        memory
    };

    assert_eq!(read(&chain(8), &["level-0"]).await?.iter().count(), 8);
    let error = read(&chain(9), &["level-0"]).await.err().ok_or("nothing failed")?;
    assert!(
        matches!(&error, LibraryError::TooDeep { limit: 8, library } if library.as_str() == "level-8"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_that_would_take_the_snapshot_past_a_limit_is_refused_before_it_is_read() -> TestResult {
    let memory = Memory::new().simple("crm", &[]);
    let named = names(&["crm"])?;

    let mut files_full = SnapshotBudget::default();
    for index in 0..499 {
        files_full.charge(&format!("pre-{index}"), 1)?;
    }
    let error = read_libraries(&named, &memory, &mut files_full)
        .await
        .err()
        .ok_or("nothing failed")?;
    assert!(
        matches!(&error, LibraryError::OverLimit { path, .. } if path == "client.rhai"),
        "{error:?}"
    );
    assert!(
        !memory.read_log().contains(&"crm/client.rhai".to_owned()),
        "the module was read past the file limit"
    );

    let mut bytes_full = SnapshotBudget::default();
    let manifest_length = u64::try_from(manifest_naming(&[]).len())?;
    bytes_full.charge(
        "pre",
        crate::library_resolution::MAX_SNAPSHOT_BYTES - manifest_length - 5,
    )?;
    let error = read_libraries(&named, &memory, &mut bytes_full)
        .await
        .err()
        .ok_or("nothing failed")?;
    assert!(
        matches!(&error, LibraryError::OverLimit { path, .. } if path == "client.rhai"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_source_that_cannot_be_asked_is_an_outage_and_not_a_missing_library() -> TestResult {
    let memory = Memory::new().simple("crm", &[]).unreachable();

    let error = read(&memory, &["crm"]).await.err().ok_or("nothing failed")?;

    assert!(matches!(error, LibraryError::Unreachable(_)), "{error:?}");
    Ok(())
}

#[tokio::test]
async fn a_workflow_that_names_no_library_reads_nothing() -> TestResult {
    let memory = Memory::new().simple("crm", &[]);

    let libraries = read(&memory, &[]).await?;

    assert_eq!(libraries.iter().count(), 0);
    assert_eq!(memory.read_log().len(), 0);
    Ok(())
}
