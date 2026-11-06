# Installation

ContextOS ships as `contextos` (the MCP server), `contextos-web` (the web interface), and `contextos-launcher`, which installs, updates, and removes the suite, and goes on your `PATH` under both programs' names to start the active release set.

## From a release

Download the archive for your platform from [Releases](https://github.com/zentelligence/contextos/releases) and unpack it. From the unpacked folder:

```sh
./bin/contextos-launcher install
```

On Windows, run `bin\contextos-launcher.exe install`. Then open a new terminal.

`install` downloads and verifies the channel's current release set, keeps it in `~/.local/share/contextos` on Linux and macOS or `%LOCALAPPDATA%\contextos` on Windows, and sets up the commands. `CONTEXTOS_INSTALL_ROOT` chooses another folder for the release sets. `--version` installs an exact release, for CI and automation, and must name the channel's current release.

The commands are copies of the launcher named `contextos`, `contextos-web`, and `contextos-launcher`, in `~/.local/bin` on Linux and macOS and `%LOCALAPPDATA%\contextos\bin` on Windows. The launcher behaves according to its file name: under a program's name it passes every argument to that program, and under any other name it manages the installation, so a downloaded copy saved as `contextos-launcher (1)` still works.

If the folder is not on your `PATH`, `install` adds it: in `~/.zshrc` for zsh, in `~/.bashrc` and `~/.bash_profile` or `~/.profile` for bash, and in the user `Path` on Windows. For any other shell it tells you to add the folder yourself. Everything placed outside the installation folder is recorded in `launchers.json` there. A file with a command's name that was not placed by an install is left alone and reported. Running `install` again repeats this without reinstalling.

### Updating

```sh
contextos-launcher update check
contextos-launcher update
contextos-launcher rollback
```

`update check` reports whether a newer release set is available, `update` verifies and installs it, and `rollback` returns to the previous one. Every update is checked against a signed manifest before it is installed, and it also replaces the launcher copies with the new release's launcher.

A background service or MCP host registration created through the launcher records the launcher, so it follows updates and rollbacks. Anything already running keeps its version until it restarts: use `contextos service restart` or `contextos-web service restart`, and restart any MCP host connected over stdio.

### Uninstalling

```sh
contextos-launcher uninstall
```

`uninstall` asks first, or add `--yes`. It removes both background services and the Claude Desktop registration, then the launcher copies, the `PATH` changes, and the installation folder. A launcher copy that has changed since it was placed is left and reported. Vaults and the configuration in `~/.config/contextos` (or your platform's equivalent) are kept.

## From source

You need a recent stable Rust toolchain. Check the minimum supported version in `rust-version` under `[workspace.package]` in the repository's `Cargo.toml`. From a checkout of this repository:

```sh
cargo build --workspace --release
```

or install a binary onto your `PATH`:

```sh
cargo install --path crates/contextos-mcp
cargo install --path crates/contextos-web
```

A source build does not use the launcher or the updater.

## First run

The fastest path is the guided-setup wizard:

```sh
contextos config
```

It walks through choosing or creating a vault, setting up logging, downloading the default local embedding model (optional, for semantic search), and registering `contextos` with a supported MCP host (currently Claude Desktop). On Windows and macOS, accept the log file it offers if you will run `contextos service`: a background service there has nowhere to send stderr (see [`configuration.md`](configuration.md#logging-to-a-file)). It writes `config.toml` to the platform default location (`~/.config/contextos/config.toml` on every platform, including Windows and macOS) unless `--config` or the `CONTEXTOS_MCP_CONFIG` environment variable points somewhere else.

To configure by hand instead, copy [`config.example.toml`](config.example.toml) and edit it (see [`configuration.md`](configuration.md) for the field reference), then run:

```sh
contextos --config /path/to/config.toml
```

## Verify the setup

```sh
contextos doctor
```

reports configuration validity, per-vault reachability, managed index staleness, Git recovery state, and semantic search health, without writing anything. Add `--resolve` to have it fix everything it can (a stale or missing managed index, MCP-made changes left uncommitted, or a Git repository that is absent or has no commits yet) automatically; add `--dry-run` alongside `--resolve` to preview what would change first.

## Registering with an MCP host

If you skipped the guided wizard, register (or check, or remove) the server's entry in a host's own configuration file directly:

```sh
contextos config mcp register --host claude-desktop
contextos config mcp status --host claude-desktop
contextos config mcp deregister --host claude-desktop
```

See [`cli-reference.md`](cli-reference.md) for the full command set.

## Running as a background service

To have one `contextos` always running for HTTP clients, install it as a per-user service:

```sh
contextos service install
contextos service status
```

See [`mcp-service.md`](mcp-service.md) for what it registers, what `install` checks first, and how to stop, restart, or remove it.

## The web interface

See [`web-first-launch.md`](web-first-launch.md) to configure and start `contextos-web`, and [`web-service.md`](web-service.md) to run it as a background service.
