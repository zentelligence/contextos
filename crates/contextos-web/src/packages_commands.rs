//! The `contextos-web packages list`, `inspect`, and `remove` commands: arguments in, text or one JSON document out.
//!
//! Each names the package store the same way, the vault, or `--shared` for the host's, and reaches the store through
//! [`contextos_web::package_stores`], so the two stores are listed, inspected, and changed by the one code. The one thing a
//! command writes to standard output is its result, and a refusal is part of that result; it then ends the process with
//! [`Refusal`](crate::job_authoring_commands::Refusal), which `main` turns into exit status 1.

use std::error::Error;
use std::io::Write as _;
use std::path::PathBuf;

use clap::Args;
use contextos_web::load_web_config;
use contextos_web::package_bundle::ExtractedBundle;
use contextos_web::package_op_error::PackageOpError;
use contextos_web::package_report::InstalledReport;
use contextos_web::package_standing::{ReleaseStanding, StoreStanding};
use contextos_web::package_store::{self, Installed, SavedJobs};
use contextos_web::package_store_report::{
    RebuiltJson, RebuiltReport, RebuiltText, RefusalJson, RefusalText, ReleaseJson, ReleaseText, RemovalJson,
    RemovalReport, RemovalText, StandingJson, StandingText,
};
use contextos_web::package_stores::{SharedPackages, VaultPackagesOf};
use contextos_web::shared_state::SharedStateStore;
use contextos_web::vault_bundle_install::BundlePath;

use crate::job_authoring_commands::{OutputFormat, Refusal};
use crate::job_target::{BundleTarget, PackageHome, ReleaseTarget, StoreTarget, TargetError};
use crate::one_shot_sessions::connect;

/// Which package store is listed.
#[derive(Clone, Debug, Args)]
pub(crate) struct ListArguments {
    /// The vault whose package store is listed; with `--shared` there is no word.
    #[arg(value_name = "VAULT", num_args = 0..=1)]
    target: Vec<String>,
    /// Lists this host's package store instead of a vault's.
    #[arg(long)]
    shared: bool,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    format: OutputFormat,
}

/// Which bundle is installed, and into which package store.
#[derive(Clone, Debug, Args)]
pub(crate) struct InstallArguments {
    /// The bundle: `<vault> <bundle>`, or with `--shared` `<bundle>`. A bundle is a `.tar.gz` archive or a folder holding
    /// the package's files as a bundle would.
    #[arg(value_name = "VAULT BUNDLE", num_args = 1..=2)]
    target: Vec<String>,
    /// Installs into this host's package store instead of a vault's. Needs no vault and no web configuration.
    #[arg(long)]
    shared: bool,
}

/// What `packages install` is asked to do: install into the host's store, which needs neither a vault nor the web
/// configuration, or into a vault's, which is reached through the primary server.
pub(crate) enum Installation {
    Shared(PathBuf),
    Vault(Request),
}

impl TryFrom<InstallArguments> for Installation {
    type Error = TargetError;

    fn try_from(arguments: InstallArguments) -> Result<Self, Self::Error> {
        let BundleTarget { home, bundle } = BundleTarget::try_from((arguments.target.as_slice(), arguments.shared))?;
        Ok(match home {
            PackageHome::Shared => Self::Shared(bundle),
            PackageHome::Vault(_) => Self::Vault(Request {
                home,
                action: Action::Install(bundle),
                format: OutputFormat::Text,
            }),
        })
    }
}

/// Which release is inspected, or whose ledger is rebuilt.
#[derive(Clone, Debug, Args)]
pub(crate) struct InspectArguments {
    /// The release: `<vault> <package> <version>`, or with `--shared` `<package> <version>`. With `--rebuild-ledger` the
    /// words name the store alone: `<vault>`, or none with `--shared`.
    #[arg(value_name = "VAULT PACKAGE VERSION", num_args = 0..=3)]
    target: Vec<String>,
    /// Inspects a release in this host's package store instead of a vault's.
    #[arg(long)]
    shared: bool,
    /// Rebuilds a missing ledger from the releases present, every one of which must read whole, and reports what it
    /// recorded. Refused while the store has a ledger, readable or not, and while any release is incomplete.
    #[arg(long)]
    rebuild_ledger: bool,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    format: OutputFormat,
}

/// Which release is removed.
#[derive(Clone, Debug, Args)]
pub(crate) struct RemoveArguments {
    /// The release: `<vault> <package> <version>`, or with `--shared` `<package> <version>`.
    #[arg(required = true, num_args = 2..=3, value_name = "VAULT PACKAGE VERSION")]
    target: Vec<String>,
    /// Removes a release in this host's package store instead of a vault's.
    #[arg(long)]
    shared: bool,
    /// Removes the release even when a rule keeps it: it is the current or previous release of a workflow, a job names it,
    /// a job file of its workflow cannot be read, or a lock or an unlocked choice holds it as a library. The jobs the next
    /// validation pass will move are reported.
    #[arg(long)]
    force: bool,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    format: OutputFormat,
}

/// What a command asks of a store.
enum Action {
    /// Installs a bundle, which only a vault's store takes through this module: the host's is installed without a session.
    Install(PathBuf),
    List,
    Inspect(ReleaseTarget),
    Rebuild,
    Remove {
        target: ReleaseTarget,
        force: bool,
    },
}

/// A package store, the host's or a vault's, behind the one set of operations a command has.
enum Store<'a> {
    Shared(SharedPackages),
    Vault(VaultPackagesOf<'a>),
}

impl Store<'_> {
    async fn carry_out(&self, action: Action) -> Result<Outcome, PackageOpError> {
        match (self, action) {
            (Self::Vault(store), Action::Install(bundle)) => {
                // Reading and verifying the bundle blocks, so it runs on a thread that may.
                let extracted = tokio::task::spawn_blocking(move || ExtractedBundle::try_from(BundlePath(&bundle)))
                    .await
                    .map_err(|_| PackageOpError::Host {
                        reason: "reading the bundle did not finish",
                    })??;
                Ok(Outcome::Installed(store.install(extracted).await?))
            }
            (Self::Shared(_), Action::Install(_)) => Err(PackageOpError::Host {
                reason: "a bundle is installed into the host's store without a session to a vault",
            }),
            (Self::Shared(store), Action::List) => Ok(Outcome::Standing(store.standing().await?)),
            (Self::Vault(store), Action::List) => Ok(Outcome::Standing(store.standing().await?)),
            (Self::Shared(store), Action::Inspect(target)) => {
                Ok(Outcome::Release(store.release(&target.package, &target.version).await?))
            }
            (Self::Vault(store), Action::Inspect(target)) => {
                Ok(Outcome::Release(store.release(&target.package, &target.version).await?))
            }
            (Self::Shared(store), Action::Rebuild) => Ok(Outcome::Rebuilt(store.rebuild_ledger().await?)),
            (Self::Vault(store), Action::Rebuild) => Ok(Outcome::Rebuilt(store.rebuild_ledger().await?)),
            (Self::Shared(store), Action::Remove { target, force }) => Ok(Outcome::Removal(
                store.remove(&target.package, &target.version, force).await?,
            )),
            (Self::Vault(store), Action::Remove { target, force }) => Ok(Outcome::Removal(
                store.remove(&target.package, &target.version, force).await?,
            )),
        }
    }
}

/// What a command found or did.
enum Outcome {
    Installed(Installed),
    Standing(StoreStanding),
    Release(ReleaseStanding),
    Removal(RemovalReport),
    Rebuilt(RebuiltReport),
}

/// A command line understood: what to do, and how to print the result.
pub(crate) struct Request {
    home: PackageHome,
    action: Action,
    format: OutputFormat,
}

impl TryFrom<ListArguments> for Request {
    type Error = TargetError;

    fn try_from(arguments: ListArguments) -> Result<Self, Self::Error> {
        Ok(Self {
            home: StoreTarget::try_from((arguments.target.as_slice(), arguments.shared))?.0,
            action: Action::List,
            format: arguments.format,
        })
    }
}

impl TryFrom<InspectArguments> for Request {
    type Error = TargetError;

    fn try_from(arguments: InspectArguments) -> Result<Self, Self::Error> {
        let words = (arguments.target.as_slice(), arguments.shared);
        let (home, action) = if arguments.rebuild_ledger {
            (StoreTarget::try_from(words)?.0, Action::Rebuild)
        } else {
            let target = ReleaseTarget::try_from(words)?;
            (target.home.clone(), Action::Inspect(target))
        };
        Ok(Self {
            home,
            action,
            format: arguments.format,
        })
    }
}

impl TryFrom<RemoveArguments> for Request {
    type Error = TargetError;

    fn try_from(arguments: RemoveArguments) -> Result<Self, Self::Error> {
        let target = ReleaseTarget::try_from((arguments.target.as_slice(), arguments.shared))?;
        Ok(Self {
            home: target.home.clone(),
            action: Action::Remove {
                target,
                force: arguments.force,
            },
            format: arguments.format,
        })
    }
}

/// Carries out the command, writes the result, and ends with [`Refusal`] when it could not be made. `config_path` is the
/// outcome of resolving `web.toml`'s path: only a vault's store needs it.
pub(crate) async fn run_packages_command(
    config_path: Result<PathBuf, Box<dyn Error + Send + Sync>>,
    request: Request,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let Request { home, action, format } = request;
    if let Ok(path) = &config_path
        && let Err(error) = crate::tracing_setup::initialise_command_tracing(path)
    {
        let refusal = PackageOpError::ConfigInvalid { reason: error.reason };
        write_result(&render_refusal(&refusal, format))?;
        return Err(Refusal.into());
    }
    // Boxed so the large session and server futures stay out of the command's own state.
    match Box::pin(carry_out(config_path.as_deref().ok(), (home, action))).await {
        Ok(outcome) => {
            write_result(&render(&outcome, format))?;
            Ok(())
        }
        Err(error) => {
            // Debug only: the refusal is the command's result, which the caller already has.
            tracing::debug!(code = error.code(), "a package store command was refused");
            write_result(&render_refusal(&error, format))?;
            Err(Refusal.into())
        }
    }
}

fn host() -> Result<SharedPackages, PackageOpError> {
    let unknown = |_| PackageOpError::Host {
        reason: "the host's data folder could not be determined",
    };
    Ok(SharedPackages {
        store_root: package_store::default_store_root().map_err(unknown)?,
        jobs: SavedJobs {
            jobs_root: package_store::default_jobs_root().map_err(unknown)?,
            state: SharedStateStore::new(package_store::default_state_root().map_err(unknown)?),
        },
    })
}

async fn carry_out(
    config_path: Option<&std::path::Path>,
    (home, action): (PackageHome, Action),
) -> Result<Outcome, PackageOpError> {
    match home {
        PackageHome::Shared => Store::Shared(host()?).carry_out(action).await,
        PackageHome::Vault(vault) => {
            let config_path = config_path.ok_or(PackageOpError::ConfigInvalid {
                reason: "its path could not be determined".to_owned(),
            })?;
            let config = load_web_config(config_path).map_err(|error| PackageOpError::ConfigInvalid {
                reason: crate::tracing_setup::ConfigInvalid::from(&error).reason,
            })?;
            let clients = connect(&config, config_path)
                .await
                .map_err(|_| PackageOpError::NotConnected {
                    reason: "the vault's server could not be reached",
                })?;
            let client = config
                .mcp_servers
                .first()
                .and_then(|primary| clients.get(primary.name()))
                .ok_or(PackageOpError::NotConnected {
                    reason: "no server is configured to reach the vault through",
                })?;
            Store::Vault(VaultPackagesOf {
                client: &client,
                vault: &vault,
            })
            .carry_out(action)
            .await
        }
    }
}

fn render(outcome: &Outcome, format: OutputFormat) -> String {
    match (outcome, format) {
        (Outcome::Installed(installed), _) => InstalledReport(installed).to_string(),
        (Outcome::Standing(standing), OutputFormat::Text) => StandingText(standing).to_string(),
        (Outcome::Standing(standing), OutputFormat::Json) => StandingJson(standing).to_string(),
        (Outcome::Release(release), OutputFormat::Text) => ReleaseText(release).to_string(),
        (Outcome::Release(release), OutputFormat::Json) => ReleaseJson(release).to_string(),
        (Outcome::Removal(report), OutputFormat::Text) => RemovalText(report).to_string(),
        (Outcome::Removal(report), OutputFormat::Json) => RemovalJson(report).to_string(),
        (Outcome::Rebuilt(report), OutputFormat::Text) => RebuiltText(report).to_string(),
        (Outcome::Rebuilt(report), OutputFormat::Json) => RebuiltJson(report).to_string(),
    }
}

/// Writes a refusal of an install into this host's store as every other packages command writes one, with its code, and
/// returns the failure that sets the exit status.
pub(crate) fn refuse_host_install(error: package_store::PackageStoreError) -> Box<dyn Error + Send + Sync> {
    let refusal = PackageOpError::from(error);
    match write_result(&render_refusal(&refusal, OutputFormat::Text)) {
        Ok(()) => Refusal.into(),
        Err(failure) => failure.into(),
    }
}

fn render_refusal(error: &PackageOpError, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => RefusalJson(error).to_string(),
        OutputFormat::Text => RefusalText(error).to_string(),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the one place the packages commands write their result to standard output"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}
