# Signals, the event stream, and agent liveness

Events are notes under `memory/events/` that the ContextOS server writes for things worth seeing: task, member, and vault health changes it fires itself, and the kinds a vault declares in `registry/events/` that sessions, jobs, and imports record. The web interface reads them, and writes nothing. [`mcp-tools.md`](mcp-tools.md#events) describes the events themselves, how they are fired and recorded, and how they are retained. This page describes what the web interface does with them.

Every read goes through `base_query` with an inline definition, so a vault without `memory/events.base` still works. An event is shown from its own frontmatter: its kind's label, severity, time, subject link, entity, and the fields its kind declares. For a kind the server fires, those are the fields that kind carries (for example `from` and `to` for `task.state-changed`). For a kind a vault records, they are the `required` and `optional` fields, and the `id_field`, of its definition in `registry/events/`. A field neither declares is never shown, and the note body is never read, so a value or private text placed in a note by hand never reaches a page. The fields are read as columns of the same `base_query` that finds the events, and the kind registry is read once for each request or poll, never one read per event. An event note that cannot be read (no readable `at`, no kind, or a severity that is not `info`, `warning`, or `error`) is left out and counted.

## The Signals screen

`GET /{vault}/signals/` shows three regions, newest first. The entity switcher narrows all of them to one entity.

- **Needs attention** lists the `warning` and `error` events that nothing since has cleared. An event with a subject is cleared by any later event about the same subject, except `task.moved`, since moving a task fixes nothing about it. A record of a keyed base, such as a task, is the same subject wherever it is filed: it is matched by its base and key (`subject_base` and `subject_id`), any other note by its path. A `vault.degraded` is cleared by a later `vault.recovered` with the same `code`. A `job.failed` or `job.suspended` is cleared by a later `job.recovered` for the same workflow and job. Any other event, with no subject, stays until it is swept or deleted. These clearing rules are defined once, in `contextos-events`, and the retention sweep in `contextos-mcp` applies the same ones, so a cleared warning is never shown open because the event that cleared it was swept. A recovery in the same second as the failure it recovers counts as later. Every warning and error is read, page by page, and only the events that may clear them (about their subjects, codes, or jobs) are read after the oldest of them, so neither a long list of warnings nor many unrelated later events changes the answer. The sidebar's Signals entry counts these and shows a dot while there are any; the count is kept for up to 10 seconds per vault and forgotten at once when the live reader sees an event that may change it.
- **Feed** lists every event, 50 to a page, each with the fields its kind declares, so a vault's own kinds show their fields without the product knowing them. A list shows as comma-separated items and a map as compact `{name: value}` text, and every value is escaped. The attention and background lines show the same fields, and the note body is never shown. The feed filters by kind, severity, and entity through partial swaps of the region (`GET /{vault}/signals/feed`). While the feed shows its first page, each live event that matches the kind, severity, and entity filters is added at the top, never one the list already shows, and the list keeps its newest 100; later pages take no live events.
- **Background activity** groups the events of kinds the product does not ship (anything that is not one of the kinds the server fires or the scaffold ships: `task.created`, `task.state-changed`, `task.reassigned`, `task.claim-lapsed`, `task.unknown-state`, `task.moved`, `member.state-changed`, `vault.degraded`, `vault.recovered`, `job.failed`, `job.suspended`, `job.recovered`, `job.retired`, `session.run-started`, `session.run-finished`, and `agent.delegated`, compared by exact name), such as a data load or a trade, by kind, the most recently active kind first, with each kind's newest five.

A region with nothing to show says so. The regions are also served alone as fragments at `/{vault}/signals/feed`, `/{vault}/signals/attention`, and `/{vault}/signals/background`.

## The live stream

`GET /{vault}/events/stream` is a Server-Sent Events response (`Cache-Control: no-store`) behind the same loopback Host check as every route. Every page of a vault opens one stream through HTMX's SSE extension (`static/htmx-ext-sse.js`, version 2.2.2 of `htmx-ext-sse`, licence beside it). It carries three messages:

- `signal`, one per new event, whose `id` is the event's cursor (its `at` in UTC and its path, joined with `|`) and whose data is the event as a rendered list item carrying a DOM `id`. The Signals feed prepends it, and the shell's stream panel counts it.
- `attention`, sent once after the events of a poll when any of them may change what pages derive from events: a warning or an error, an event about a subject, a recovery, or a session or member event. The attention region, the Tasks list, and the Agents list reload themselves on it.
- `status`, which tells the shell panel whether the server can be read.
- `reload`, after a reconnect that missed more than 500 events. The Signals regions, the Tasks list, and the Agents list reload themselves on it.

Every host session runs its own `contextos` process, so no single process sees every event to push it. The web interface therefore keeps one reader per vault, which runs only while at least one stream is open. Each `[events] poll_seconds` it asks `contextos` for events at or after the start of its cursor's second, in the current month's folder (and the previous month's near a month boundary, or from the cursor's month when the cursor is older), and sends every one it has not already delivered, rendered once, to every open stream of that vault. Several events can share a second and their paths do not sort in the order they were written, so it remembers the paths it has delivered in the newest second rather than trusting the cursor alone. A new reader treats the events in the newest existing event's second as already delivered, or starts from the current whole second when there is none, so a new stream is not sent history. Each read is bounded below by the start of the newest delivered event's second, however long the vault has been quiet, and excludes only the paths in that second, so the cost of a poll does not grow with the age of the cursor or the number of warnings already shown. It stops as soon as the last stream closes, and every stream ends promptly when the server shuts down. A stream that falls too far behind is ended, and its browser reconnects.

A reconnecting browser sends the last `id` it saw as `Last-Event-ID`, and every event after it is sent first, oldest first, up to the reader's own position (the whole of the cursor's own second is sent again but for the cursor's event, since an event written after it may sort before it, and a page ignores any it already shows); when more than 500 were missed, one `reload` message is sent instead and the page reloads its regions, so nothing missed is lost silently. A page may also open its first stream with `?after=` and the cursor of the newest event it shows, so none is missed between rendering and connecting. A cursor that cannot be read is ignored. `?entity=` limits a stream to one entity, as the entity switcher does, matching the entity exactly as stored. An unknown `severity` on the Signals screen or the feed region is refused with `400` and `route/malformed-query`.

While `contextos` cannot be reached, including when the stream is opened, the stream answers and stays open, sends a comment line each interval so a proxy keeps it alive, and the panel shows disconnected until a poll succeeds. Events written in the meantime are sent once it can. A connected stream that is quiet sends a comment line at least every 15 seconds. A reader pages through the vault's events however many there are in a day or a second, up to 5,000 in one poll; a resume that would need more than 2,000 reads sends `reload`. A request whose `Accept` does not allow `text/event-stream` is answered `406`.

The footer's stream panel shows connected or disconnected and how many events have arrived since the page loaded. The count is made by the page's stylesheet, not by script: the only script attributes are the two HTMX `hx-on` handlers on the shell that note the browser's connection opening or failing.

## The Tasks attention indicator

A task shows a dot, amber for a warning and red for an error, when the most recent event about the task is a warning or an error. An event is about a task when it names the task base and the task's key (`subject_base` and `subject_id`), so a task keeps its events wherever it is filed, and a record of another base with the same key is not the task. Any later event about the task, of any severity, clears it, because it becomes the most recent, except `task.moved`. A task with no key has no indicator. On the Signals screen, an event about a task opens the task's own page by its key; any other subject opens the note where it was when the event happened. A task with no events shows none, and if events cannot be read the Tasks screen renders without indicators. The indicator is read when the screen renders and the list reloads itself when the stream sends `attention`, so a lapsed claim or an unknown state turns a task amber or red without a page reload.

## Agents liveness

Beside its state, each agent member shows, when any session event names the member:

- the time and severity of the last `session.run-finished` whose `members` include it;
- how many tasks are claimed for it, meaning `claimed_by` begins `{slug}/` and `claimed_until` has not passed; and
- amber, naming the run id and its start time, for each `session.run-started` that names the member and has no `session.run-finished` with the same `run_id`, when either every task whose `claimed_by` ends `/{run id}` has a `claimed_until` in the past, or no task holds such a claim and the start is older than `[agents] run_overdue_minutes`. A run whose tasks were all finished, and so released, falls under the second condition, because only a finish ends a run. At most three overdue runs are named.

Any `task.claim-lapsed` event in the last 7 days for a task assigned to the member is listed under it. A member no session event names shows no liveness and no amber. Liveness is read when the screen renders and never decides the member's state.

## Configuration

Both settings are in `web.toml`, read each time they are used, so a saved change applies without a restart (a change to `poll_seconds` applies to the next reader to start):

```toml
[events]
poll_seconds = 5

[agents]
run_overdue_minutes = 120
```

`poll_seconds` defaults to 5 and must be at least 1. `run_overdue_minutes` defaults to 120 and must be at least 1. An unknown key in either table is rejected.

## Events the web scheduler records

The scheduler records `job.failed`, `job.suspended`, `job.recovered`, and `job.retired` for vault jobs, so a crashed job is still shown. See "Events recorded for job outcomes" in [`web-workflows.md`](web-workflows.md).

## Alerts

The top-bar bell counts what needs the operator, and opens the list; the Overview's Alert rail shows the same list. `GET /{vault}/alerts` answers with the list (a fragment for an HTMX request, else a page) and `GET /{vault}/alerts/summary` with the bell's count. Both are read on every request from what the vault already holds; nothing of their own is stored. An alert is one of:

- an open task whose `due` date is before today (the operator's local date): a `due` that does not begin with a `YYYY-MM-DD` date is never read as one;
- an open question: an operational question (a `question` task in `memory/tasks.base` that is still in an open state) or a vault question (a question record in the Decisions register, `memory/decisions.base` by default, whose `status` is `open`); and
- a `warning` or `error` event nothing has cleared, as the Signals screen's Needs attention lists them.

A task in a `waiting` state is not an alert: waiting names something outside the vault. Alerts are listed most recent first, by the event's time, the question's last change, or the task's due date, up to 50 with the rest counted. A source that cannot be read is left out and the list says it may be incomplete. Both the list and the count reload themselves when the stream sends `attention`.

## The vault Overview

The vault root is the Overview dashboard. Task lifecycle renders with the page; every other panel loads after it as its own region and reloads itself on `attention`, so one slow or unreadable source never holds up the dashboard.

- **Bridge** shows the team member flagged `chief_of_staff = true` in `.contextos/team.toml`: their state, how many open tasks are assigned to them, how many they hold claimed, when their last session finished and how, and their lapsed claims (from the Agents screen's liveness), with a link to the Decisions register. With no one flagged, it says how to flag one. (`GET /{vault}/overview/bridge`)
- **System status** shows the MCP servers' connections and tools, vault health, the live stream's connection, and the last 24 hours of events by severity, each opening its screen. (`GET /{vault}/overview/status`)
- **Task lifecycle** carries the stage strip, whose Attention tile counts the tasks with an open warning or error (as the Tasks screen's indicator does) and opens Signals. A task's state alone never raises attention.
- **Needs a call from you** is the Signals screen's Needs attention region.
- **Alert rail** is the alerts list above.
- **Inputs** lists one row per source: the Inbox (items waiting), each job that runs on a schedule (its last run and how it ended), and each kind of event a source records rather than the server firing it (its newest event and its severity), each opening its screen. (`GET /{vault}/overview/inputs`)

The footer ends with an about line: the publisher, Zentelligence, linking its site, and the running version.
