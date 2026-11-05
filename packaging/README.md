# ContextOS

ContextOS gives AI assistants safe, structured access to your ContextOS knowledge vault: a folder of Markdown notes with a defined structure, which also opens in Obsidian. It keeps a recoverable history of every change, keeps indexes up to date, and adds a web interface for reading your vault, running apps, and scheduling workflows.

This package needs no other software to run. No Rust or developer tools are required.

## What is in this package

| File | What it is |
| --- | --- |
| `bin/contextos` | The MCP server. AI assistants connect to it to read, search, and change your vaults. It also holds the setup, health check, and update commands. |
| `bin/contextos-web` | The web interface, served on your own machine. It shows your vaults, hosts your apps, and runs your scheduled workflows. |
| `bin/contextos-launcher` | The small program you put on your `PATH`. It starts whichever installed version is active, so updates take effect without changing your setup. |

On Windows, each of these ends in `.exe`.

## Install

Open a terminal in the folder you unpacked this package into. In the commands below, replace `VERSION` with the version in the package's file name, such as `0.23.7`.

**Linux and macOS**

```sh
./bin/contextos install --version VERSION
mkdir -p ~/.local/bin
cp bin/contextos-launcher ~/.local/bin/contextos
cp bin/contextos-launcher ~/.local/bin/contextos-web
```

If `~/.local/bin` is not on your `PATH`, add it in your shell's profile.

**Windows (PowerShell)**

```powershell
.\bin\contextos.exe install --version VERSION
$bin = "$env:LOCALAPPDATA\Programs\ContextOS"
New-Item -ItemType Directory -Force $bin
Copy-Item bin\contextos-launcher.exe "$bin\contextos.exe"
Copy-Item bin\contextos-launcher.exe "$bin\contextos-web.exe"
$path = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$path;$bin", "User")
```

Then open a new terminal and check that it works:

```sh
contextos --version
```

The installed versions are kept in `~/.local/share/contextos` on Linux and macOS, and in `%LOCALAPPDATA%\contextos` on Windows. You can delete the unpacked folder afterwards.

These programs are not signed with an Apple or Microsoft certificate, so macOS or Windows may warn you before running them the first time. On macOS, allow them under System Settings, Privacy & Security. On Windows, choose More info, then Run anyway.

## Set up

1. Run `contextos config`. It walks you through choosing or creating a vault, logging, the optional semantic search model, and connecting to Claude Desktop.
2. Run `contextos doctor` to check your configuration and vaults. Add `--resolve` to fix what it can.
3. Run `contextos-web config` and add `contextos` as a stdio server, first in the list.
4. Start the web interface with `contextos-web`, or run `contextos-web service install` to have it start whenever you log in.
5. Open <http://127.0.0.1:7373/settings/> and check that the server reads **Connected**.

The web interface is only reachable from your own machine.

## Connect your AI assistant

The ContextOS plugin teaches your assistant how to work with a vault: setting it up, capturing and researching knowledge, and building apps and workflows. It is published in the [Zentelligence marketplace](https://github.com/zentelligence/marketplace).

In Claude Code:

```text
/plugin marketplace add zentelligence/marketplace
/plugin install context-os@zentelligence-plugins
```

Then run `/vault init` in your vault to finish first-run setup. The marketplace page also covers Codex and ChatGPT desktop.

## What you can do with the web interface

- **Read your vault:** notes, Bases, Canvases, diagrams, a daily journal, and live activity, all rendered in your browser.
- **Run your own apps:** a web app kept in your vault under `registry/apps/` appears in the interface, and can use your vault through the MCP server.
- **Schedule workflows:** a workflow is a script kept in your vault or installed for the whole machine. A saved job runs it on a schedule or on demand, and every run is recorded.

## Keep up to date

```sh
contextos update check
contextos update
```

`contextos rollback` returns to the previous version.

A background service keeps running the version it started with, so after updating, run `contextos service restart` or `contextos-web service restart`. Restart any assistant connected over stdio too.

## Learn more

- [Installation and first run](https://github.com/zentelligence/contextos/blob/main/docs/installation.md)
- [Command reference](https://github.com/zentelligence/contextos/blob/main/docs/cli-reference.md)
- [Configuration](https://github.com/zentelligence/contextos/blob/main/docs/configuration.md)
- [Running workflows](https://github.com/zentelligence/contextos/blob/main/docs/web-workflows.md)
- [Report a problem](https://github.com/zentelligence/contextos/issues)

## Licence

Apache 2.0. Copyright Zentelligence Pty Ltd. See `LICENSE`, `NOTICE`, and `THIRD_PARTY_LICENSES.md`.
