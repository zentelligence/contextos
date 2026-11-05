# Installation

ContextOS ships as `contextos` (the MCP server), `contextos-web` (the web interface), and `contextos-launcher`, which goes on your `PATH` under both command names and starts the active release set.

## From a release

Download the archive for your platform from [Releases](https://github.com/zentelligence/contextos/releases) and unpack it. No Rust toolchain is needed. From the unpacked folder, replacing `VERSION` with the version in the archive's name:

```sh
./bin/contextos install --version VERSION
mkdir -p ~/.local/bin
cp bin/contextos-launcher ~/.local/bin/contextos
cp bin/contextos-launcher ~/.local/bin/contextos-web
```

On Windows, run `bin\contextos.exe install --version VERSION`, then copy `bin\contextos-launcher.exe` to `contextos.exe` and `contextos-web.exe` in a folder on your `PATH`. The README in the archive gives the PowerShell commands.

Release sets are kept in `~/.local/share/contextos` on Linux and macOS, and `%LOCALAPPDATA%\contextos` on Windows. `CONTEXTOS_INSTALL_ROOT` chooses another folder.

### Updating

```sh
contextos update check
contextos update
contextos rollback
```

`update check` reports whether a newer release set is available, `update` verifies and installs it, and `rollback` returns to the previous one. Every update is checked against a signed manifest before it is installed.

A background service or MCP host registration records the executable that created it. After updating, repeat `contextos service install`, `contextos-web service install`, or `contextos config mcp register` so that it uses the new release set.

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
