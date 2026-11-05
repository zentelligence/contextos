use std::fs;
use std::path::Path;

use contextos_core::{VaultRoot, VaultRootInput};
use contextos_fs::{FilesystemConfig, FsLimits};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::NameRejection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    first: TempDir,
    second: TempDir,
    source: VaultSetSource,
}

fn write(root: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let mut roots = Vec::new();
    for (path, name) in [(first.path(), "alpha"), (second.path(), "beta")] {
        roots.push(VaultRoot::try_from(VaultRootInput {
            path: path.to_path_buf(),
            managed: false,
            name: Some(name.to_owned()),
        })?);
    }
    let roots = VaultSet::try_from(roots)?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![
            FsLimits {
                max_read_bytes: 1 << 20,
                max_batch_files: 16,
            };
            2
        ],
        hidden: vec![Vec::new(); 2],
        atomic_write_guard: None,
    })?;
    Ok(Fixture {
        first,
        second,
        source: VaultSetSource::new(roots, filesystem),
    })
}

fn vault(fixture: &Fixture, index: usize) -> Result<SourceVault, Box<dyn std::error::Error>> {
    fixture
        .source
        .vaults()
        .into_iter()
        .nth(index)
        .ok_or_else(|| "no such vault".into())
}

fn slug(text: &str) -> Result<Slug, ProjectionError> {
    Slug::try_from(text)
}

#[test]
fn the_vaults_are_reported_in_configuration_order_with_their_names() -> TestResult {
    let fixture = fixture()?;
    let names: Vec<String> = fixture.source.vaults().into_iter().map(|vault| vault.name).collect();
    assert_eq!(names, ["alpha", "beta"]);
    Ok(())
}

#[test]
fn agent_names_list_markdown_files_except_the_index_and_report_invalid_names() -> TestResult {
    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "registry/agents/reviewer.md", "x")?;
    write(root, "registry/agents/index.md", "x")?;
    write(root, "registry/agents/notes.txt", "x")?;
    write(root, "registry/agents/Bad Name.md", "x")?;
    fs::create_dir_all(root.join("registry/agents/folder.md"))?;
    let mut names = fixture.source.agent_names(&vault(&fixture, 0)?)?;
    names.sort_by_key(|name| format!("{name:?}"));
    assert_eq!(
        names,
        vec![
            DefinitionName::Invalid {
                raw: "Bad Name".to_owned(),
                reason: NameRejection::InvalidCharacter
            },
            DefinitionName::Valid(slug("reviewer")?),
        ]
    );
    Ok(())
}

#[test]
fn a_vault_without_a_registry_defines_nothing() -> TestResult {
    let fixture = fixture()?;
    let vault = vault(&fixture, 1)?;
    assert_eq!(fixture.source.agent_names(&vault)?, vec![]);
    assert_eq!(fixture.source.skill_names(&vault)?, vec![]);
    assert_eq!(fixture.source.read_agent(&vault, &slug("reviewer")?)?, None);
    assert!(fixture.source.read_skill(&vault, &slug("writing")?)?.is_none());
    Ok(())
}

#[test]
fn an_agent_is_read_byte_for_byte_from_its_own_vault() -> TestResult {
    let fixture = fixture()?;
    write(fixture.second.path(), "registry/agents/reviewer.md", "beta copy\r\n")?;
    assert_eq!(
        fixture.source.read_agent(&vault(&fixture, 0)?, &slug("reviewer")?)?,
        None
    );
    assert_eq!(
        fixture.source.read_agent(&vault(&fixture, 1)?, &slug("reviewer")?)?,
        Some(b"beta copy\r\n".to_vec())
    );
    Ok(())
}

#[test]
fn skill_names_list_folders_only() -> TestResult {
    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "registry/skills/writing/SKILL.md", "x")?;
    write(root, "registry/skills/index.md", "x")?;
    write(root, "registry/skills/Bad_Name/SKILL.md", "x")?;
    let mut names = fixture.source.skill_names(&vault(&fixture, 0)?)?;
    names.sort_by_key(|name| format!("{name:?}"));
    assert_eq!(
        names,
        vec![
            DefinitionName::Invalid {
                raw: "Bad_Name".to_owned(),
                reason: NameRejection::InvalidCharacter
            },
            DefinitionName::Valid(slug("writing")?),
        ]
    );
    Ok(())
}

#[test]
fn a_skill_folder_is_read_with_its_nested_files() -> TestResult {
    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "registry/skills/writing/SKILL.md", "main")?;
    write(root, "registry/skills/writing/references/style.md", "style")?;
    let folder = fixture
        .source
        .read_skill(&vault(&fixture, 0)?, &slug("writing")?)?
        .ok_or("skill not found")?;
    let files: Vec<(String, Vec<u8>)> = folder
        .files()
        .map(|(path, bytes)| (<&str>::from(path).to_owned(), bytes.to_vec()))
        .collect();
    assert_eq!(
        files,
        vec![
            ("SKILL.md".to_owned(), b"main".to_vec()),
            ("references/style.md".to_owned(), b"style".to_vec()),
        ]
    );
    Ok(())
}

#[test]
fn an_empty_skill_folder_is_an_invalid_folder_not_an_absent_one() -> TestResult {
    let fixture = fixture()?;
    fs::create_dir_all(fixture.first.path().join("registry/skills/writing"))?;
    let result = fixture.source.read_skill(&vault(&fixture, 0)?, &slug("writing")?);
    assert!(matches!(result, Err(ProjectionError::InvalidSkillFolder { .. })));
    Ok(())
}

#[cfg(unix)]
#[test]
fn links_are_refused_wherever_they_sit_in_a_definition() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "registry/agents/real.md", "x")?;
    write(root, "registry/skills/writing/SKILL.md", "x")?;
    write(root, "elsewhere/data.md", "secret")?;
    symlink(
        root.join("registry/agents/real.md"),
        root.join("registry/agents/linked.md"),
    )?;
    symlink(
        root.join("elsewhere/data.md"),
        root.join("registry/skills/writing/inner.md"),
    )?;
    symlink(root.join("elsewhere"), root.join("registry/skills/redirected"))?;
    let vault = vault(&fixture, 0)?;
    assert!(matches!(
        fixture.source.read_agent(&vault, &slug("linked")?),
        Err(ProjectionError::SourceLink { .. })
    ));
    assert!(matches!(
        fixture.source.read_skill(&vault, &slug("writing")?),
        Err(ProjectionError::SourceLink { .. })
    ));
    assert!(matches!(
        fixture.source.read_skill(&vault, &slug("redirected")?),
        Err(ProjectionError::SourceLink { .. })
    ));
    assert!(fixture.source.read_agent(&vault, &slug("real")?)?.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_registry_folder_that_is_a_link_is_refused() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "elsewhere/reviewer.md", "x")?;
    fs::create_dir_all(root.join("registry"))?;
    symlink(root.join("elsewhere"), root.join("registry/agents"))?;
    let vault = vault(&fixture, 0)?;
    assert!(matches!(
        fixture.source.agent_names(&vault),
        Err(ProjectionError::SourceLink { .. })
    ));
    assert!(matches!(
        fixture.source.read_agent(&vault, &slug("reviewer")?),
        Err(ProjectionError::SourceLink { .. })
    ));
    Ok(())
}

#[test]
fn a_directory_where_an_agent_file_should_be_is_a_read_failure() -> TestResult {
    let fixture = fixture()?;
    fs::create_dir_all(fixture.first.path().join("registry/agents/reviewer.md"))?;
    let result = fixture.source.read_agent(&vault(&fixture, 0)?, &slug("reviewer")?);
    assert!(matches!(result, Err(ProjectionError::SourceRead { .. })));
    Ok(())
}

#[test]
fn a_skill_with_more_files_than_the_limit_is_an_invalid_folder() -> TestResult {
    let fixture = fixture()?;
    for index in 0..=MAX_SKILL_FILES {
        write(fixture.first.path(), &format!("registry/skills/big/f{index}.md"), "x")?;
    }
    let result = fixture.source.read_skill(&vault(&fixture, 0)?, &slug("big")?);
    assert!(matches!(result, Err(ProjectionError::InvalidSkillFolder { .. })));
    Ok(())
}

#[test]
fn the_global_instructions_are_read_from_the_registry_and_absence_is_none() -> TestResult {
    let fixture = fixture()?;
    write(fixture.first.path(), "registry/instructions/global.md", "Orient.\n")?;
    assert_eq!(
        fixture.source.read_orientation(&vault(&fixture, 0)?)?,
        Some(b"Orient.\n".to_vec())
    );
    assert_eq!(fixture.source.read_orientation(&vault(&fixture, 1)?)?, None);
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_global_instructions_are_never_read_through_a_link() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "elsewhere/secret.md", "secret")?;
    fs::create_dir_all(root.join("registry/instructions"))?;
    symlink(
        root.join("elsewhere/secret.md"),
        root.join("registry/instructions/global.md"),
    )?;
    assert!(matches!(
        fixture.source.read_orientation(&vault(&fixture, 0)?),
        Err(ProjectionError::SourceLink { .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_global_instructions_folder_that_is_a_link_is_refused() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "elsewhere/global.md", "secret")?;
    fs::create_dir_all(root.join("registry"))?;
    symlink(root.join("elsewhere"), root.join("registry/instructions"))?;
    assert!(matches!(
        fixture.source.read_orientation(&vault(&fixture, 0)?),
        Err(ProjectionError::SourceLink { .. })
    ));
    Ok(())
}

fn skill_file(fixture: &Fixture, relative: &str, length: u64) -> TestResult {
    let path = fixture.first.path().join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(path)?.set_len(length)?;
    Ok(())
}

#[test]
fn a_skill_nested_more_than_eight_levels_is_an_invalid_folder() -> TestResult {
    let fixture = fixture()?;
    let nine = (1..=9)
        .map(|level| format!("level-{level}"))
        .collect::<Vec<_>>()
        .join("/");
    write(fixture.first.path(), "registry/skills/deep/SKILL.md", "x")?;
    write(
        fixture.first.path(),
        &format!("registry/skills/deep/{nine}/note.md"),
        "x",
    )?;

    let result = fixture.source.read_skill(&vault(&fixture, 0)?, &slug("deep")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::InvalidSkillFolder {
                reason: "it nests folders too deeply"
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_skill_nested_exactly_eight_levels_is_read() -> TestResult {
    let fixture = fixture()?;
    let eight = (1..=8)
        .map(|level| format!("level-{level}"))
        .collect::<Vec<_>>()
        .join("/");
    write(fixture.first.path(), "registry/skills/deep/SKILL.md", "x")?;
    write(
        fixture.first.path(),
        &format!("registry/skills/deep/{eight}/note.md"),
        "x",
    )?;

    assert!(
        fixture
            .source
            .read_skill(&vault(&fixture, 0)?, &slug("deep")?)?
            .is_some()
    );
    Ok(())
}

#[test]
fn a_skill_holding_more_than_thirty_two_mebibytes_in_all_is_an_invalid_folder() -> TestResult {
    let fixture = fixture()?;
    // Thirty-three files just under the per-file read limit: each is allowed alone, together they are over 32 MiB.
    for index in 0..33 {
        skill_file(
            &fixture,
            &format!("registry/skills/heavy/part-{index}.bin"),
            (1 << 20) - 1,
        )?;
    }
    write(fixture.first.path(), "registry/skills/heavy/SKILL.md", "x")?;

    let result = fixture.source.read_skill(&vault(&fixture, 0)?, &slug("heavy")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::InvalidSkillFolder {
                reason: "it holds too many bytes"
            })
        ),
        "{result:?}"
    );
    Ok(())
}

/// A source with the largest per-file read limit, so a skill is refused only by its own limits.
fn roomy_source(dir: &Path) -> Result<VaultSetSource, Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: dir.to_path_buf(),
        managed: false,
        name: Some("roomy".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 10 << 20,
            max_batch_files: 16,
        }],
        hidden: vec![Vec::new()],
        atomic_write_guard: None,
    })?;
    Ok(VaultSetSource::new(roots, filesystem))
}

fn sparse(path: &Path, length: u64) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::File::create(path)?.set_len(length)?;
    Ok(())
}

#[cfg(unix)]
fn unreadable_sparse(path: &Path, length: u64) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    sparse(path, length)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000))?;
    Ok(())
}

fn roomy_vault(source: &VaultSetSource) -> Result<SourceVault, Box<dyn std::error::Error>> {
    source.vaults().into_iter().next().ok_or_else(|| "no such vault".into())
}

#[cfg(unix)]
#[test]
fn a_source_skill_whose_listed_sizes_exceed_the_total_is_refused_before_any_file_is_read() -> TestResult {
    let dir = tempdir()?;
    let source = roomy_source(dir.path())?;
    // Neither file can be opened, so a read would fail with a read error instead of the limit error.
    for name in ["a", "b", "c", "d"] {
        unreadable_sparse(&dir.path().join(format!("registry/skills/heavy/{name}.bin")), 9 << 20)?;
    }

    let result = source.read_skill(&roomy_vault(&source)?, &slug("heavy")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::InvalidSkillFolder {
                reason: "it holds too many bytes"
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_single_source_skill_file_over_the_per_file_limit_is_refused_without_being_read() -> TestResult {
    let dir = tempdir()?;
    let source = roomy_source(dir.path())?;
    unreadable_sparse(&dir.path().join("registry/skills/heavy/big.bin"), 11 << 20)?;

    let result = source.read_skill(&roomy_vault(&source)?, &slug("heavy")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::SourceRead { ref source, .. }) if matches!(**source, FsError::TooLarge { .. })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_managed_index_at_any_level_of_a_source_skill_is_not_part_of_the_skill() -> TestResult {
    let fixture = fixture()?;
    let root = fixture.first.path();
    write(root, "registry/skills/triage/SKILL.md", "x")?;
    write(root, "registry/skills/triage/scripts/run.sh", "y")?;
    let plain = fixture
        .source
        .read_skill(&vault(&fixture, 0)?, &slug("triage")?)?
        .ok_or("no skill")?;
    write(root, "registry/skills/triage/index.md", "managed")?;
    write(root, "registry/skills/triage/scripts/index.md", "managed")?;
    write(root, "registry/skills/triage/scripts/deep/index.md", "managed")?;

    let indexed = fixture
        .source
        .read_skill(&vault(&fixture, 0)?, &slug("triage")?)?
        .ok_or("no skill")?;

    let names: Vec<&str> = indexed.files().map(|(path, _)| <&str>::from(path)).collect();
    assert_eq!(names, ["SKILL.md", "scripts/run.sh"]);
    assert_eq!(
        contextos_core::ContentHash::from(&indexed),
        contextos_core::ContentHash::from(&plain)
    );
    Ok(())
}

#[test]
fn a_managed_index_does_not_count_towards_the_skill_size_or_file_limits() -> TestResult {
    let dir = tempdir()?;
    let source = roomy_source(dir.path())?;
    write(dir.path(), "registry/skills/light/SKILL.md", "x")?;
    for index in ["index.md", "scripts/index.md", "a/index.md", "b/index.md"] {
        sparse(&dir.path().join("registry/skills/light").join(index), 9 << 20)?;
    }
    for index in 0..511 {
        write(dir.path(), &format!("registry/skills/light/part-{index}.txt"), "x")?;
    }

    let result = source.read_skill(&roomy_vault(&source)?, &slug("light")?);

    let skill = result?.ok_or("no skill")?;
    assert_eq!(skill.files().count(), 512);
    Ok(())
}
