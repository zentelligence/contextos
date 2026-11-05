# Running the web interface as a service

`contextos-web service` installs the web interface as a background service that starts when you log in, runs as you, and needs no Administrator or root rights. It reads the same `--config` (or `CONTEXTOS_WEB_CONFIG`) as the server, and the installed service starts the server with that `web.toml`.

| Command | What it does |
| --- | --- |
| `contextos-web service install` | Registers the service and starts it. Running it again updates the registration and leaves a running server running. |
| `contextos-web service uninstall` | Stops the server and removes the registration. |
| `contextos-web service status` | Reports whether the service is installed, and whether its server is running, stopped, or stopped unexpectedly. |
| `contextos-web service start` | Starts the installed service's server. Leaves a running server alone and says so. |
| `contextos-web service stop` | Stops the server until you start it again or next log in. |
| `contextos-web service restart` | Stops the server if it is running, then starts it. |

## Platforms

| Platform | Mechanism | Restarts by itself after a crash |
| --- | --- | --- |
| Linux | A `systemd --user` unit, `contextos-web.service` | Yes (`Restart=on-failure`) |
| macOS | A `launchd` agent, `au.zentelligence.contextos-web` | Yes (`KeepAlive`) |
| Windows | A Startup App: the `ContextOS Web` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | No |
| Windows, `install --scheduled-task` | A Scheduled Task, `ContextOS Web`, with a logon trigger | No |

On macOS, `stop` unloads the agent, since `KeepAlive` would otherwise start the server straight back up; `start` loads it again.

On Windows, the server runs under `%SystemRoot%\System32\conhost.exe --headless`, so no console window opens and nothing needs to stay open. That needs Windows 10 1809 or later. Because no console is shown, its log lines belong in a file: `contextos-web config` offers `%LOCALAPPDATA%\contextos\logs\contextos-web.log`, rotated at 10 MB and kept for 7 days, when `web.toml` sets no `log_file`. On macOS the `launchd` agent sends stderr nowhere, so it offers `~/Library/Logs/contextos/contextos-web.log` there by the same rule.

The Scheduled Task is there for an operator who prefers Task Scheduler. Task Scheduler only lets an Administrator terminal register or remove a task, so `install --scheduled-task` and a later `uninstall` must run from one. Installing either mechanism removes the other: with both registered, two servers would start at logon and the second would fail on the same port. The step that needs an Administrator always comes first, so if it is refused nothing else has changed and the existing service keeps running. `uninstall`, `status`, `start`, `stop`, and `restart` act on whichever one is installed, and refuse, naming the registered command, when it runs the server with a different `web.toml` from the one the command was given: run the command again with that `--config`. `install` refuses the same way rather than replace a registration for another `web.toml`, whose server would keep running with nothing left to stop it: uninstall that one first, with its `--config`.

The Windows commands run every tool by its full path under `%SystemRoot%\System32`, and read the registration and the server's process back through Windows PowerShell with UTF-8 output, so paths that are not plain ASCII (such as `C:\Users\José`) compare correctly. A PowerShell restricted to Constrained Language Mode (by AppLocker or WDAC) cannot run them.

Windows refuses a logon command longer than it accepts (260 characters for a Startup App, 261 for a Scheduled Task), so `install` checks the length first and names it. Move `contextos-web` or its `web.toml` to a shorter path if it is refused.

## Recovering after a crash

On Linux and macOS the service manager restarts a crashed server. On Windows, `contextos-web service status` reports a server that stopped unexpectedly, and `contextos-web service start` (or `restart`) starts it again.

Windows tracks no process for a Startup App, and matching processes by name would also catch a `contextos-web` command you are running yourself. So once it is listening, the server records its process id, its executable, and the time it wrote the record beside the `web.toml` it loaded (`web.toml.process.json` for `web.toml`), and removes the record when it stops cleanly. Each configuration's server has its own record. The service commands act only on the process that record names, and only when that process runs the recorded executable (whichever copy of `contextos-web` runs the command) and started before the record was written; a process id Windows has since given to another process, even another `contextos-web`, belongs to a process that started later:

- no record: the server is stopped;
- a record whose process is the server: the server is running;
- any other record: the server stopped unexpectedly. `start` clears the record and starts a new server; `stop` clears it without ending any process.

A record that cannot be read is reported with its path rather than taken to mean "no server"; stop any running `contextos-web`, delete the file, and run the command again.

`stop` ends the server's process outright, as Task Scheduler does, then waits up to five seconds for it to exit so `restart`'s new server can take its port; see [`web-shutdown.md`](web-shutdown.md) for what that means for requests and jobs in flight. The record relies on the clock: a system clock set back between a crash and Windows reusing the server's process id could make a later `contextos-web` from the same executable look like the server.
