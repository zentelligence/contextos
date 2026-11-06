# Web interface: first launch and MCP connections

`contextos-web` starts and serves its setup pages whether or not any vault exists and whether or not any MCP server it is configured with can be reached. Each configured server's connection is shown on its own, with the reason it is not connected and what fixes it, and a server is connected again from `/server/settings/` without restarting the process.

## What works with nothing configured

With no `[[mcp_server]]` entry in `web.toml`, or with every entry failing to connect, these stay available:

| Page or route | Purpose |
| --- | --- |
| `/server/settings/` | Adding, editing, and removing MCP servers, appearance, and each server's connection |
| `/shell/status` | The header badge and footer count of connected servers, polled by every page |
| `/api/v1/server/packages` | Installing, listing, inspecting, and removing workflow packages in the host's own store; a vault's store is at `/api/v1/{vault}/packages` |
| `/server/workflows`, `/server/jobs`, `/server/packages` | Shared workflows, their jobs, and library releases, kept in the host's own app-data directory |

`/` sends you to `/server/settings/` until the primary server (the first `[[mcp_server]]` entry, the local `contextos` server) is connected and reports at least one vault. A vault page asked for while the primary server is not connected answers `502` `mcp/unreachable`; with no server configured at all it answers `500` `mcp/server-not-configured`.

Installing a workflow package and running a shared job need only the host's own storage and configuration, never a vault's `registry/workflows/` or `memory/` folders, so the workflow that creates the first vault can be installed before that vault exists.

## First launch

1. Run `contextos config` to create `config.toml`. It may name a vault folder, which can be empty, or none yet.
2. Run `contextos-web config` and add the `contextos` server as a stdio entry, first in the list: the first entry is the primary server every vault page uses.
3. Start the web interface, as a service with `contextos-web service install` (see [`web-service.md`](web-service.md)) or directly with `contextos-web`.
4. Open `/server/settings/` and check that each server reads **Connected**. Fix any that do not, using the remedy each one shows.
5. Install the workflow package that sets up your vault, and run its setup job.

Start-up attempts every configured server at once and serves pages once each attempt has ended, after at most a minute each. A server that cannot be connected is logged at `WARN` with its cause and shown in `/server/settings/`; it never stops start-up. The command-line runners (`contextos-web jobs run` and `contextos-web workflows run`) are the exception: a run cannot go ahead without its stdio servers, so they stop with the error of the first stdio server that cannot start.

## Connection readiness

Each server on `/server/settings/` shows one of these:

| State | Meaning | What to do |
| --- | --- | --- |
| Connected | The session is open and answered just now; the panel counts its tools | Nothing |
| Connecting | An attempt is under way; the panel updates itself when it ends | Wait |
| Unreachable | The session opened, but the server has stopped answering since | Reconnect |
| Login required | The server needs a credential it does not have, or rejected the one it was given | Sign in (OAuth2), paste a new token under Edit (bearer), or choose an authentication mode under Edit |
| Not connected | The last attempt failed; the panel shows the error | Follow the remedy shown, then save or Reconnect |

An OAuth2 server whose stored token cannot be refreshed reads as **Login required** from the next call that finds it so. It reads as connected again once a later refresh succeeds, for example after the provider was briefly unreachable, or once you sign in again, which also connects it again by itself. Pasting a new bearer token under Edit and saving connects that server again too.

A bearer token taken from an environment variable (`token_env`) is read from `contextos-web`'s own environment, which only a restart changes: either set the variable and restart, or paste the token under Edit instead.

## Server tokens

An HTTP server's bearer token comes from either an environment variable or a pasted token, chosen when you add the server or later under Edit. An OAuth2 client secret, for a provider that issues one, works the same way under Edit: a variable name (`client_secret_env`) or a pasted secret (`client_secret_ref`). Neither is a fallback of the other. A pasted token, like an OAuth2 client secret, is kept in the `credentials/` folder beside `web.toml`, never in `web.toml` itself; `web.toml` records only the name it is stored under (`credential_ref`) or the variable's name (`token_env`). `[mcp_server.headers]` is for values that are not secret, since it is written in `web.toml` as shown: put a token under Bearer token instead.

Giving both a variable name and a pasted token is refused (`settings/pasted-secret-refused`) rather than one being kept silently, as is an empty pasted value or a server name that cannot name a stored secret (one containing `/`, `\`, or `..`). If the credentials folder cannot be written, the entry is still saved and the page says the token was not stored, so paste it again once the folder is writable.

`/server/settings/` shows each configured secret hidden. **Show** beside it reveals that one value: a pasted token or client secret as stored, or a variable's value as `contextos-web` currently sees it (or that the variable is not set). **Hide** hides it again. A revealed value is never cached by the browser. Only the page's own request can ask for one, and, like every request to `contextos-web`, it must name a loopback host (`localhost`, `127.0.0.1`, or `[::1]`): a request naming any other host is refused with `403` `route/host-not-allowed`, so a page on another site cannot reach `contextos-web` by making its own name resolve to this machine.

Workflow readiness is separate from connection readiness. A workflow or job that names a server that is not connected is still listed and can still be validated; a run that calls that server fails with `workflow/server-unavailable` (see [`web-workflows.md`](web-workflows.md)).

## Connecting a server again

**Reconnect** on a server's panel reads `web.toml` again and opens a new session from the entry it holds now, so an entry corrected by hand is connected as it now reads. The old session keeps serving until the new one is open, then closes once the calls running on it finish, allowing them up to 30 seconds. If the new attempt fails, the old session is closed and the panel shows why.

Saving an `[[mcp_server]]` entry on `/server/settings/` applies it at once: a new entry is connected, a changed one is connected again, and a removed one is closed. An unchanged entry keeps its session.

Removing an entry checks first that no registered app still names it, which reads the vaults through the primary server. While the primary server is not connected that check cannot run, so the removal is refused with `502` `mcp/unreachable`; correct the primary's entry, or reconnect it, first. When the primary server is connected but its vault list cannot be read or understood, the removal is refused with `502` `mcp/vault-list-unreadable`, and the detail says why.

Nothing reconnects on its own: a server that keeps failing is never relaunched in a loop. Reconnect when the cause is fixed.

## Adding or removing a vault

`contextos` reads `config.toml` when it starts. To have a vault you added with `contextos config vault add`, or removed with `contextos config vault remove`, take effect:

- for a stdio `contextos` server, which `contextos-web` launches, press **Reconnect** on its panel: the server is launched again and reads the new configuration;
- for an HTTP `contextos` server, restart that server, then press **Reconnect**.

`contextos-web` reads the vault list from the server on every request, so the next page shows the change. Removing the last vault sends `/` back to `/server/settings/`.

## What still needs a restart

- `[server]` settings in `web.toml`: the bind address, logging, and the static files directory.
- A change to an environment variable named by `token_env`.
