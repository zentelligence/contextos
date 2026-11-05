# HTTP Routing & Response Contract Standard

## Overview

This specification defines the URL schema and response contracts for applications serving one or more of the following surfaces:

- **HTML**: Full-page server-rendered responses for direct browser navigation
- **HTMX**: HTML fragment responses for HTMX-driven interactions
- **API**: JSON responses for programmatic clients (mobile, third-party, service-to-service)

Applications implement one or more surfaces depending on their requirements. The schema and contracts are consistent across all apps using this specification.

## Namespaces

|Namespace|Surface |Primary Consumer           |
|---------|--------|---------------------------|
|`/`      |HTML    | Browser (direct navigation)|
|`/api/v{n}`|JSON API| Programmatic clients       |
|`/mcp/v{n}`|MCP API| Agentic clients       |

HTMX interactions are **not** a separate namespace. HTMX requests hit the same `/` routes as browser navigation. The server differentiates using the `HX-Request: true` header sent automatically by HTMX on every request.

- `HX-Request` absent or false: return a full HTML page
- `HX-Request: true`: return an HTML fragment

## URL Schema

Never use a command when other operations can already achieve the outcome in a standardised way.

### HTML + HTMX Surface (`/`)

|Path                                |Method|Query Params              |Purpose                      |Returns                               |
|------------------------------------|------|--------------------------|-----------------------------|--------------------------------------|
|`/{entities}`                       |GET   |pagination, simple filters|List entities                |Full page or fragment                 |
|`/{entities}/new`                   |GET   |none                      |Load create form             |Form fragment                         |
|`/{entities}/{id}`                  |GET   |none                      |View entity                  |Full page or fragment                 |
|`/{entities}/{id}/edit`             |GET   |none                      |Load edit form               |Pre-populated form fragment           |
|`/{entities}/{id}/confirm-delete`   |GET   |none                      |Load delete confirmation     |Confirmation fragment                 |
|`/{entities}`                       |POST  |none                      |Submit create form           |Entity fragment or error form         |
|`/{entities}/{id}`                  |PATCH |none                      |Submit partial update        |Updated entity fragment or error form |
|`/{entities}/{id}`                  |PUT   |none                      |Submit full update           |Updated entity fragment or error form |
|`/{entities}/{id}`                  |DELETE|none                      |Delete entity                |Empty response or replacement fragment|
|`/{entities}/execute/{command}`     |POST  |none                      |Execute command on collection|Result fragment or error fragment     |
|`/{entities}/{id}/execute/{command}`|PUT   |none                      |Execute command on entity    |Result fragment or error fragment     |

Sub-entity nesting can be supported e.g. /{entities}/{id}/{sub-entities}/{id} etc with same schema at each level

### JSON API Surface (`/api/v1`)

|Path                                       |Method|Query Params              |Purpose                      |Returns    |
|-------------------------------------------|------|--------------------------|-----------------------------|-----------|
|`/api/v1/{entities}`                       |GET   |pagination, simple filters|List entities                |JSON array |
|`/api/v1/{entities}/query`                 |POST  |none                      |Complex querying             |JSON array |
|`/api/v1/{entities}`                       |POST  |none                      |Create entity                |JSON entity|
|`/api/v1/{entities}/{id}`                  |GET   |none                      |Retrieve entity              |JSON entity|
|`/api/v1/{entities}/{id}`                  |PATCH |none                      |Partial update               |JSON entity|
|`/api/v1/{entities}/{id}`                  |PUT   |none                      |Full update                  |JSON entity|
|`/api/v1/{entities}/{id}`                  |DELETE|none                      |Delete entity                |Empty      |
|`/api/v1/{entities}/execute/{command}`     |POST  |none                      |Execute command on collection|JSON result|
|`/api/v1/{entities}/{id}/execute/{command}`|PUT   |none                      |Execute command on entity    |JSON result|

Sub-entity nesting to be supported e.g. /{entities}/{id}/{sub-entities}/{id} etc with same schema at each level

## Response Contracts

### HTML Surface (no `HX-Request` header)

- **Status 200**: Full HTML page, including layout, navigation, and content.
- **Status 404**: Full HTML 404 page.
- **Status 500**: Full HTML error page.
- **Redirects**: Use standard HTTP 302. Do not use `HX-Redirect` on full-page responses.

### HTMX Surface (`HX-Request: true`)

- **Status 200**: HTML fragment only. No `<html>`, `<head>`, or `<body>` tags. No layout or navigation chrome.
- **Status 200 (form error)**: Re-render the form fragment with inline validation messages. Do not use 4xx for validation errors on HTMX form submissions; return 200 with the error state embedded in the fragment so HTMX performs the swap.
- **Status 204**: Empty response. Used for DELETE when the element should be removed via `hx-swap="outerHTML"` with no replacement.
- **Status 422**: Unprocessable. Use only for non-form errors where a fragment cannot be meaningfully rendered.
- **Redirects**: Use `HX-Redirect` response header with a 200 status to trigger full-page navigation from an HTMX interaction. Do not use HTTP 302 for HTMX responses.
- **Error fragments**: Render inline or as a toast fragment. Never return a bare error string.

### JSON API Surface

- **Status 200**: JSON body for successful GET, PATCH, PUT, and command operations.
- **Status 201**: JSON body for successful POST (entity created). Include `Location` header pointing to the new resource.
- **Status 204**: Empty body for successful DELETE.
- **Status 200 for DELETE (exception)**: A DELETE whose removal has something the caller must be told answers `200` with a JSON body instead of `204`. The package store's guarded removal, `DELETE /api/v1/{vault}/packages/{name}/versions/{version}` and the same under `shared`, does so when retention could not finish afterwards (the body carries `warnings`) or when retention removed further releases that only the removed one kept (the body carries `retention_removed`). A removal with neither answers `204`.
- **Status 400**: JSON error body for malformed requests. Schema: `{ "error": { "code": string, "message": string, "fields": { [field]: string[] } } }`. `fields` is optional, used for validation errors.
- **Status 401**: JSON error body. Authentication required.
- **Status 403**: JSON error body. Authenticated but not authorised.
- **Status 404**: JSON error body. Resource not found.
- **Status 409**: JSON error body. Conflict (e.g. duplicate, state violation).
- **Status 422**: JSON error body. Request is well-formed but semantically invalid.
- **Status 500**: JSON error body. Internal error. Do not leak internal details.

JSON error body schema:

```json
{
  "error": {
    "code": "VALIDATION_FAILED",
    "message": "One or more fields are invalid.",
    "fields": {
      "title": ["is required"],
      "email": ["is not a valid email address"]
    }
  }
}
```

## Server-Sent Events

A live stream is an HTML surface route that answers a request accepting `text/event-stream` with a Server-Sent Events response, read in the browser through HTMX's SSE extension (`sse-connect`, `sse-swap`, and `hx-trigger="sse:{event}"`) so no client script is written for it. It is not a separate namespace, and it is still behind the same Host check as every other route.

|Path                       |Method|Query Params                         |Purpose                                  |Returns                                   |
|---------------------------|------|-------------------------------------|-----------------------------------------|------------------------------------------|
|`/{vault}/events/stream`   |GET   |`entity`, `after` (a cursor)         |Stream a vault's new events as they happen|`text/event-stream`, `Cache-Control: no-store`|

- Each message names its event (`signal`, `attention`, or `status`), and the data of a `signal` or `status` message is an HTML fragment on one line. A `signal` message's `id` is a cursor the browser sends back as `Last-Event-ID` when it reconnects, so the server resumes from it.
- A request whose `Accept` header does not allow `text/event-stream` is answered `406`. An unknown resource is `404`, as for any route.
- Comment lines keep the response open while the server has nothing to say. The server ends a stream it cannot keep up with, so the browser reconnects, and never buffers without bound.
- Regions of a screen that a stream refreshes are ordinary collection sub-resources that return fragments, such as `GET /{vault}/signals/feed`, and are reloaded with `hx-get` on `sse:attention`.

## HTMX-Specific Constraints

- HTML forms must not use `method="put"` or `method="patch"`. HTML only supports GET and POST natively. Use `hx-put` or `hx-patch` attributes on the `<form>` element instead.
- Delete actions use `hx-delete` on a button or element directly. No `/delete` sub-path is required.
- For delete confirmation UX, use `GET /{entities}/{id}/confirm-delete` to load a confirmation fragment inline, rather than `hx-confirm` (browser native dialog). The confirmation fragment contains the `hx-delete` trigger.
- `hx-boost` is not used. All HTMX interactions are explicit via `hx-get`, `hx-post`, `hx-patch`, `hx-put`, `hx-delete` attributes.

## Pagination (HTML and API)

Query parameters for list endpoints:

|Param  |Type   |Description                           |
|-------|-------|--------------------------------------|
|`page` |integer|Page number, 1-indexed. Default: 1.   |
|`limit`|integer|Items per page. Default: 20. Max: 100.|
|`sort` |string |Field name to sort by.                |
|`order`|string |`asc` or `desc`. Default: `asc`.      |

API list responses include a `meta` envelope:

```json
{
  "data": [...],
  "meta": {
    "page": 1,
    "limit": 20,
    "total": 143,
    "pages": 8
  }
}
```

HTMX list fragments include pagination controls as part of the fragment.

## Surface Implementation Matrix

Apps implement only the surfaces they require. The schema is consistent regardless of which surfaces are active.

|App Type  |Implement `/`|Implement `/api/v1`|
|----------|-------------|-------------------|
|UI only   |Yes          |No                 |
|API only  |No           |Yes                |
|Full-stack|Yes          |Yes                |

## Naming Conventions

- `{entities}` is always **plural**, lowercase, hyphenated. Examples: `posts`, `content-items`, `user-profiles`.
- `{id}` is the entity’s permanent identifier. Prefer v7 UUIDs.
- `{command}` is a verb or verb-noun phrase, lowercase, hyphenated. Examples: `publish`, `archive`, `send-notification`.
- Sub-paths `/new`, `/edit`, `/rename` (when id is a filepath), and `/confirm-delete` are reserved and must not be used as entity identifiers.
