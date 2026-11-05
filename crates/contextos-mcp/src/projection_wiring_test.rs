use std::path::PathBuf;

use contextos_fs::FilesystemConfig;

use super::*;
use crate::{Config, ConfigLocation, ConfigProvenance};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const AGENT: &str = "---\ndescription: Reviews code\ntier: balanced\naccess: all\n---\n\nReview it.\n";

struct World {
    _directory: tempfile::TempDir,
    home: PathBuf,
    vault: PathBuf,
}

fn world() -> Result<World, BoxError> {
    let directory = tempfile::tempdir()?;
    let base = dunce::canonicalize(directory.path())?;
    let home = base.join("home");
    let vault = base.join("vault");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(vault.join("registry/agents"))?;
    std::fs::write(vault.join("registry/agents/reviewer.md"), AGENT)?;
    Ok(World {
        _directory: directory,
        home,
        vault,
    })
}

impl World {
    /// Makes a host's home folder, as installing the host would.
    fn install(&self, home_folder: &str) -> Result<(), BoxError> {
        std::fs::create_dir_all(self.home.join(home_folder))?;
        Ok(())
    }

    /// Every name directly under the temporary home, sorted.
    fn home_entries(&self) -> Result<Vec<String>, BoxError> {
        let mut names = std::fs::read_dir(&self.home)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, BoxError>>()?;
        names.sort();
        Ok(names)
    }

    fn input(&self, location: ConfigLocation) -> Result<ProjectionWiringInput, BoxError> {
        let source = format!("[[vault]]\npath = {:?}\n", self.vault.display().to_string());
        let mut config = Config::try_from(source.as_str())?;
        config.provenance = ConfigProvenance {
            location,
            path: Some(self.home.join("config.toml")),
            home: Some(self.home.clone()),
        };
        let roots = VaultSet::try_from(&config)?;
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![contextos_fs::FsLimits {
                max_read_bytes: 5 * 1024 * 1024,
                max_batch_files: 50,
            }],
            hidden: vec![Vec::new()],
            atomic_write_guard: None,
        })?;
        Ok(ProjectionWiringInput {
            setup: ProjectionSetup::try_from(&config)?,
            roots,
            filesystem,
        })
    }
}

#[test]
fn a_disabled_setup_is_disabled_and_touches_no_host_folder() -> Result<(), BoxError> {
    let world = world()?;

    let projection = Projection::from(world.input(ConfigLocation::Named)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Disabled));
    assert_eq!(
        std::fs::read_dir(&world.home)?.count(),
        0,
        "a disabled configuration created something"
    );
    Ok(())
}

#[test]
fn installed_hosts_get_their_missing_target_folders_and_refresh_projects_what_the_vault_holds() -> Result<(), BoxError>
{
    let world = world()?;
    world.install(".claude")?;
    world.install(".codex")?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Enabled(_)));
    assert!(world.home.join(".claude/agents/reviewer.md").is_file());
    assert!(world.home.join(".codex/agents/reviewer.toml").is_file());
    assert!(world.home.join(".claude/skills").is_dir() && world.home.join(".agents/skills").is_dir());
    assert!(world.home.join(".contextos/projection.lock").is_file());
    Ok(())
}

#[test]
fn an_absent_codex_home_creates_nothing_for_codex_and_claude_still_projects() -> Result<(), BoxError> {
    let world = world()?;
    world.install(".claude")?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Enabled(_)));
    assert!(world.home.join(".claude/agents/reviewer.md").is_file());
    assert_eq!(world.home_entries()?, [".claude", ".contextos"]);
    Ok(())
}

#[test]
fn a_present_codex_home_with_no_agents_folder_gets_the_user_skills_folder_created() -> Result<(), BoxError> {
    let world = world()?;
    world.install(".codex")?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Enabled(_)));
    assert!(world.home.join(".agents/skills").is_dir());
    assert!(world.home.join(".codex/agents/reviewer.toml").is_file());
    assert!(
        !world.home.join(".claude").exists(),
        "an absent Claude Code home was created"
    );
    Ok(())
}

#[test]
fn with_no_host_installed_projection_is_disabled_and_the_home_is_untouched() -> Result<(), BoxError> {
    let world = world()?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Disabled));
    assert!(world.home_entries()?.is_empty(), "something was created under the home");
    Ok(())
}

#[test]
fn a_host_whose_folders_cannot_be_prepared_is_skipped_and_the_other_still_projects() -> Result<(), BoxError> {
    let world = world()?;
    world.install(".claude")?;
    world.install(".codex")?;
    std::fs::write(
        world.home.join(".claude/agents"),
        b"a file where Claude Code's folder belongs",
    )?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);
    projection.refresh();

    assert!(matches!(projection, Projection::Enabled(_)));
    assert!(world.home.join(".codex/agents/reviewer.toml").is_file());
    assert!(
        world.home.join(".claude/agents").is_file(),
        "the blocking file was disturbed"
    );
    Ok(())
}

#[test]
fn with_every_installed_host_unpreparable_projection_is_disabled_rather_than_failing() -> Result<(), BoxError> {
    let world = world()?;
    world.install(".claude")?;
    world.install(".codex")?;
    std::fs::write(world.home.join(".claude/agents"), b"in the way")?;
    std::fs::write(world.home.join(".codex/agents"), b"in the way")?;

    let projection = Projection::from(world.input(ConfigLocation::PlatformDefault)?);

    assert!(matches!(projection, Projection::Disabled));
    Ok(())
}

/// Collects everything a subscriber writes, so a test can assert on what was logged.
#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut inner) = self.0.lock() {
            inner.extend_from_slice(buffer);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl LogCapture {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|inner| String::from_utf8_lossy(&inner).into_owned())
            .unwrap_or_default()
    }
}

#[test]
fn a_doctor_projection_over_a_host_with_no_agents_folder_still_assesses_that_hosts_skills_and_logs_no_warning()
-> Result<(), BoxError> {
    let world = world()?;
    world.install(".claude")?;
    std::fs::create_dir_all(world.home.join(".claude/skills/writing"))?;
    std::fs::write(
        world.home.join(".claude/skills/writing/SKILL.md"),
        b"left by an earlier projection",
    )?;
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let input = world.input(ConfigLocation::PlatformDefault)?;
    let projection = tracing::subscriber::with_default(subscriber, || Projection::for_doctor(input));

    let logged = capture.text();
    assert!(
        !logged.contains("WARN"),
        "assessing a host with a missing folder logged a warning: {logged}"
    );
    assert!(
        matches!(projection, Projection::Enabled(_)),
        "the host was left out because one of its folders is missing"
    );
    assert!(
        !world.home.join(".claude/agents").exists(),
        "composing for doctor created a folder"
    );
    Ok(())
}
