# Documentation

Usage documentation for the `contextos` MCP server.

- [`installation.md`](installation.md): building, installing, and first run
- [`configuration.md`](configuration.md): `config.toml` field reference (see also [`config.example.toml`](config.example.toml), a complete annotated example)
- [`cli-reference.md`](cli-reference.md): every `contextos` subcommand
- [`mcp-service.md`](mcp-service.md): running the MCP server as a background service over HTTP: installing, starting, stopping, and recovering it
- [`mcp-tools.md`](mcp-tools.md): the MCP tools the server registers
- [`search.md`](search.md): full-text, semantic, and blended search: paging, sorting, relevance, and degradation

- [`link-expansion.md`](link-expansion.md): how `fs_attach_file` and `frontmatter_read` resolve every link in a note to a vault path
- [`rendering.md`](rendering.md): how the web interface renders notes, Base, Canvas, and diagram files: the pipeline, the dialect, the fence vocabulary, and the author HTML allowlist
- [`web-links.md`](web-links.md): how the web interface routes links to the screens that own them, and shows them short
- [`web-journal.md`](web-journal.md): the Journal screen, how a day's tabs come from its entry and the vault's template, section editing, and `.contextos/journal.toml`
- [`web-vault-settings.md`](web-vault-settings.md): the vault settings screen, its management categories, and the `.contextos/vault-management.toml` and `.contextos/tasks.toml` files it edits
- [`web-workflows.md`](web-workflows.md): running workflows and jobs from the command line, their inputs, scoping, and failure codes
- [`web-signals.md`](web-signals.md): the Signals screen, the live event stream and its shell panel, the Tasks attention indicator, and Agents liveness
- [`web-first-launch.md`](web-first-launch.md): starting the web interface with no vault or no working MCP server, each server's connection readiness, and reconnecting a server or picking up a vault without a restart
- [`web-service.md`](web-service.md): running the web interface as a background service: installing, starting, stopping, and recovering it after a crash
- [`web-shutdown.md`](web-shutdown.md): graceful web and MCP session shutdown, deadlines, and systemd settings

## API and code reference

Rustdoc comments on every public type and function are the source of truth for implementation detail. Generate a browsable copy locally with:

```sh
cargo doc --workspace --no-deps --open
```

Nothing generated is committed to this repository.
