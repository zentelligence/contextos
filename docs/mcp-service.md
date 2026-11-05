# Running the MCP server as a service

`contextos service` registers one background `contextos` that serves MCP over streamable HTTP and nothing else. It starts when you log in, runs as you, and needs no Administrator or root rights. Clients that would otherwise each start their own stdio process (several agent sessions, or a session in WSL reaching a vault that lives on Windows) connect to it instead. The web interface has its own service, described in [`web-service.md`](web-service.md); the two are independent and use different names.

The service runs `contextos --config <absolute path> --http [ADDR] --no-stdio`. `--config` is resolved the way the server resolves it: the `--config` flag, then `CONTEXTOS_MCP_CONFIG`, then `~/.config/contextos/config.toml`. The address is the global `--http ADDR` given to `install`; with none, the service runs a bare `--http`, so the `bind` in `[server.http]` (default `127.0.0.1:3737`) applies at each start.

| Command | What it does |
| --- | --- |
| `contextos service install` | Checks the configuration, registers the service, and starts it. Running it again updates the registration and leaves a running server running. |
| `contextos service uninstall` | Stops the server and removes the registration. |
| `contextos service status` | Reports whether the service is installed, and whether its server is running, stopped, or stopped unexpectedly. |
| `contextos service start` | Starts the installed service's server. Leaves a running server alone and says so. |
| `contextos service stop` | Stops the server until you start it again or next log in. |
| `contextos service restart` | Stops the server if it is running, then starts it. |

For example, `contextos --http 127.0.0.1:9000 service install` registers a service that serves on port 9000.

## What `install` checks

A service that cannot start is never registered. Before it registers anything, `install` loads the configuration exactly as the service will (this file, the HTTP override, no stdio) and checks the bind with the same rules the server applies. It refuses, and changes nothing, when:

- the configuration file does not exist (run `contextos config`, or name the right file with `--config`);
- the configuration does not load, for example invalid TOML or a vault that cannot be used;
- the address is not a `host:port`; or
- the address is not a loopback address and neither `token` nor `token_env` is configured.

The service manager does not hand the service your shell's environment, so a token supplied only through `CONTEXTOS_MCP_TOKEN` in your shell would pass at install and be missing at every start. For that reason `install` ignores the variable: put the token in `[server.http]`, or bind to a loopback address. `token_env` names a variable the service reads from its own environment at every start, so it is accepted at install; make sure the service manager provides it (for example `~/.config/environment.d/` for `systemd --user`, or a user environment variable on Windows), because the service refuses to start while it is unset.

## Platforms

| Platform | Mechanism | Restarts by itself after a crash |
| --- | --- | --- |
| Linux | A `systemd --user` unit, `contextos.service` | Yes (`Restart=on-failure`) |
| macOS | A `launchd` agent, `au.zentelligence.contextos` | Yes (`KeepAlive`) |
| Windows | A Startup App: the `ContextOS MCP` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | No |
| Windows, `install --scheduled-task` | A Scheduled Task, `ContextOS MCP`, with a logon trigger | No |

The mechanisms, their Administrator requirements on Windows, the headless console host, the command length limits, and crash recovery are the same as for the web interface; [`web-service.md`](web-service.md) describes them. Everywhere it names `contextos-web` or `web.toml`, read `contextos` and `config.toml`.

`start` and `restart` always run the server exactly as it was registered, on every platform, so `--http ADDR` given at install keeps applying without being repeated. To change the address, install again with the new one.

## One server per address

Only one process on a host can hold the HTTP port. A second `contextos --http` on an address already in use stops at once and tells you to run `contextos service status`. Stdio-only `contextos` processes are unaffected and may run beside the service.

The process serving HTTP records its process id, its executable, and the time it wrote the record in `config.toml.process.json`, beside the configuration it loaded, once it is listening, and removes the record when it stops cleanly. On Windows the service commands use it to find, stop, and report on the server, exactly as described in the web interface's page. Stdio-only processes never write it.

## Pointing a client at the service

Use the streamable HTTP URL `http://127.0.0.1:3737/mcp` (or your address) in the client's MCP configuration. A non-loopback bind requires the configured token as a bearer token.
