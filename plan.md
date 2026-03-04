# Dynamic API Tool Discovery

## Goal

Let the agent discover and use arbitrary remote APIs from within a session — no user configuration needed. Given a URL, the agent connects, handles auth, discovers endpoints, and starts calling them. Minimize human involvement: the human should only be asked to do things a machine genuinely can't (sign into a website, solve a CAPTCHA, paste a key).

## Design Principles

1. **One call to connect.** `api_connect` should do everything it can autonomously — fetch the spec, auto-detect auth requirements, check for stored credentials, probe `.well-known` endpoints — and only return to the agent when it needs something it can't resolve alone.
2. **Auth is internal.** `phyl-tool-api` reads and writes credentials from disk (`secrets.env`, `tokens/`). The agent never sees raw tokens. When the tool needs the human (OAuth consent, paste a key), it says so in its response and the agent relays via `ask_human`.
3. **No duplicate tools.** Secrets management lives in one place (`phyl-tool-secrets`). `phyl-tool-api` uses `secrets.env` and `tokens/` directly — it doesn't expose its own secret CRUD tools.
4. **The agent can define tools itself.** For APIs without machine-readable specs, the agent reads the docs (via existing `web_read` tool), figures out the endpoints, and registers them explicitly via `api_register_tools`.
5. **Cookie jar per connection.** Each registered API gets a persistent `reqwest` cookie jar. This makes session-based auth, CSRF tokens, and post-CAPTCHA state actually work.

## Component Changes

### 1. New crate: `phyl-tool-api` (server-mode)

Provides these built-in tools on startup via `--spec`:

#### `api_connect` — Discover and register an API

```
Parameters:
  name: string       — Prefix for generated tool names (e.g., "acme")
  url: string        — Base URL, OpenAPI spec URL, or MCP endpoint URL
  headers: object    — Optional extra headers for all requests to this API
```

**`api_connect` does all of this internally, in order:**

1. **Check for existing connection** — if `name` is already registered, return its current tool list immediately.
2. **Check for stored auth** — look for `$PHYLACTERY_HOME/tokens/{name}.json` (OAuth) or a secret key matching `{NAME}_API_KEY` / `{NAME}_TOKEN` in `secrets.env`. If found, load it.
3. **Spec auto-discovery** — try fetching, in order:
   - The URL as given (might be an OpenAPI spec directly)
   - `{url}/openapi.json`, `{url}/openapi.yaml`
   - `{url}/swagger.json`, `{url}/v2/swagger.json`
   - `{url}/.well-known/openid-configuration` (OAuth discovery)
   - `{url}/.well-known/oauth-authorization-server`
   - The URL as an MCP endpoint (try Streamable HTTP, then SSE)
   - The URL as a plain HTML page (return content for the agent to interpret)
4. **Parse the spec** — OpenAPI 3.x, Swagger 2.x, MCP tools/list, or "no spec found."
5. **Extract auth requirements** — from OpenAPI `securitySchemes`, or from the OAuth discovery document. Store the auth metadata (type, URLs, scopes) in the connection state.
6. **Generate ToolSpecs** — one per endpoint, prefixed with `{name}_`.
7. **Test the connection** — make a lightweight probe request (e.g., GET the base URL or a health endpoint) with stored credentials (if any). If it returns 401/403, note that auth is needed.
8. **Return a structured result:**

```json
{
  "status": "connected" | "auth_required" | "no_spec",
  "tools_registered": 12,
  "tools": ["acme_list_users", "acme_get_user", ...],
  "auth": {
    "type": "oauth2" | "api_key" | "bearer" | "basic" | "none" | "unknown",
    "has_credentials": true,
    "oauth": {                          // present only if type == "oauth2"
      "authorize_url": "https://...",
      "token_url": "https://...",
      "device_authorize_url": "https://...",  // null if not supported
      "scopes_available": ["read", "write", "admin"],
      "provider_name": "GitHub"         // extracted from discovery doc
    },
    "instructions": "..."              // human-readable: what the agent should do next
  },
  "spec_format": "openapi_3" | "swagger_2" | "mcp" | "html" | "none"
}
```

When `status == "connected"`, tools are emitted via `tools_changed` signal and the agent can start using them immediately.

When `status == "auth_required"`, no `tools_changed` signal yet. The `auth` block tells the agent exactly what to do. The agent then calls `api_auth` (see below) and reconnects.

When `status == "no_spec"`, the agent can use `api_register_tools` to define endpoints manually, or use `api_call` for ad-hoc requests.

#### `api_auth` — Resolve auth for a connection

```
Parameters:
  name: string                — Registered API name
  method: "oauth_code" | "oauth_device" | "api_key" | "bearer" | "basic"
  scopes: string[]            — For OAuth flows (optional, defaults to all available)
  client_id: string           — For OAuth (optional — uses built-in if available, asks human if not)
  client_secret: string       — For OAuth (optional)
```

**Behavior by method:**

- **`oauth_code`**: Starts a temporary localhost callback server. Constructs the authorization URL with PKCE. Returns the URL and a `wait_id`. The response tells the agent to send the URL to the human. While the human authenticates, `phyl-tool-api` listens on the callback port in a background thread. The agent should call `api_auth_poll` to check for completion (or the tool can block and use the NDJSON multiplexing — the `id` field means other requests can still be processed).

  Actually, simpler: `api_auth` **blocks on this request ID** until the callback arrives or a timeout (5 minutes) expires. Since server-mode tools multiplex by `id`, `phyl-run` can still dispatch other tool calls to `phyl-tool-api` while this one is pending. The tool just holds the response for this particular `id` until the OAuth callback fires. No polling needed.

- **`oauth_device`**: Requests a device code, returns the verification URL and user code. Blocks (same as above) while polling the token endpoint every 5 seconds until the user completes auth or timeout.

- **`api_key`** / **`bearer`** / **`basic`**: Returns a response telling the agent what to ask the human for. The agent calls `ask_human`, gets the value, then calls `secret_store` (from `phyl-tool-secrets`). Then calls `api_connect` again — it will pick up the newly stored credential.

**On success**: stores tokens in `$PHYLACTERY_HOME/tokens/{name}.json`, then automatically re-runs the connection probe. If the probe succeeds, emits `tools_changed` with the full tool set. Returns success with the tool list.

**On failure**: returns error with enough detail for the agent to diagnose (expired client_id, user denied consent, wrong scopes, etc.).

#### `api_register_tools` — Manually define endpoints

For APIs without machine-readable specs. The agent reads docs and creates tool definitions:

```
Parameters:
  name: string       — API name (must already be api_connect'd for base URL + auth)
  tools: [
    {
      "tool_name": "list_users",           — becomes {name}_list_users
      "description": "List all users",
      "method": "GET",
      "path": "/api/v1/users",
      "query_params": { "page": "integer", "per_page": "integer" },
      "body_schema": null,
      "response_hint": "JSON array of user objects"
    },
    ...
  ]
```

Generates `ToolSpec` entries from the provided definitions. Emits `tools_changed`. This is how the agent handles non-OpenAPI APIs: it reads the docs with `web_read`, figures out the endpoints, and registers them.

#### `api_call` — Ad-hoc HTTP request against a registered API

```
Parameters:
  name: string       — Registered API name (uses its base URL, auth, cookie jar)
  method: string     — GET, POST, PUT, DELETE, PATCH, HEAD
  path: string       — Path relative to base URL (or absolute URL)
  query: object      — Query parameters
  body: object       — Request body (JSON)
  headers: object    — Extra headers for this request only
```

Uses the connection's auth, cookie jar, and default headers. Returns status code, response headers (selected subset), and body. This is different from `phyl-tool-web`'s `http_fetch` because it inherits the API connection's auth and state.

#### `api_disconnect` — Remove a registered API

```
Parameters:
  name: string
```

Drops the connection state, cookie jar, and generated tools. Emits `tools_changed`. Does NOT delete stored credentials (they persist for future sessions).

#### Dynamic endpoint tools — Generated per-API

When the agent calls `acme_list_users(page: 2)`, `phyl-tool-api`:
1. Looks up the endpoint definition for `acme_list_users`
2. Builds the HTTP request from the spec (method, path template, param placement)
3. Applies auth from the connection state (bearer token, API key, etc.)
4. Auto-refreshes OAuth tokens if expired
5. Sends request through the connection's cookie jar
6. On success: returns `{ "status": 200, "body": {...}, "pagination": {"next": "...", "has_more": true} }`
7. On 401: attempts token refresh, retries once, then returns auth error
8. On 403/429 with challenge: returns `human_challenge` structured error (see below)
9. On 429 with Retry-After: waits and retries (up to 3 times), then returns rate limit error with timing info

### 2. Protocol extension: `tools_changed` signal

**In `phyl-core`:**

```rust
pub struct ServerResponse {
    pub id: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub signal: Option<String>,           // existing
    pub tools: Option<Vec<ToolSpec>>,     // NEW — when signal == "tools_changed"
}
```

When `signal == "tools_changed"`, `tools` contains the **complete** tool list for this server-mode tool process (all built-in tools + all dynamically generated tools across all connections). Full replacement, not diff.

**In `phyl-run`:**

After dispatching a server-mode tool call, check for `tools_changed`:

```rust
if response.signal.as_deref() == Some("tools_changed") {
    if let Some(new_tools) = response.tools {
        // Remove all specs previously owned by this executable
        all_specs.retain(|s| tool_map.get(&s.name).map(|ti| &ti.executable) != Some(&exec_path));
        tool_map.retain(|_, ti| ti.executable != exec_path);
        // Add new specs
        for spec in &new_tools {
            tool_map.insert(spec.name.clone(), ToolInfo {
                executable: exec_path.clone(),
                mode: spec.mode.clone(),
            });
        }
        all_specs.extend(new_tools);
    }
}
```

**Blocking `api_auth` and NDJSON multiplexing:** `phyl-run` currently dispatches server-mode calls sequentially. For `api_auth` (which blocks waiting for OAuth callback), this would block ALL other server-mode tools. Fix: dispatch server-mode calls with a simple approach — when a response doesn't arrive within 1 second, park the request and move on. Check parked requests after each model turn. Alternatively (simpler): spawn a dedicated thread per server-mode tool for reading responses, and use a channel to receive them. This is a bigger change to `phyl-run` but unlocks real multiplexing.

**Recommended approach for v1:** Keep sequential dispatch but add a special case for `api_auth` — since it blocks, the agent should call it as the only tool call in a turn (no parallel calls). The model naturally does this since it needs the auth result before doing anything else. Document this constraint; fix with real multiplexing later.

### 3. OpenAPI spec parsing

Use the `openapiv3` crate for OpenAPI 3.x. For Swagger 2.x, use `serde_json` to parse and convert to a minimal internal representation.

For each endpoint, generate a `ToolSpec`:
- **name**: `{api_name}_{operation_id}` if operationId exists, else `{api_name}_{method}_{path_slug}` (e.g., `acme_get_api_v1_users_by_id`)
- **description**: `{summary}. {description}` from the spec, truncated to 200 chars. Include the HTTP method and path for clarity: `"GET /users/{id} — Retrieve a user by ID"`
- **parameters**: Flatten into a single JSON Schema object. Path params, query params, and body properties all become top-level properties. Prefix with `path_`, `query_`, `body_` only if there are name collisions. Deeply nested schemas are inlined (resolved `$ref`s) and simplified: strip `example`, `x-*` extensions, collapse single-`allOf` wrappers.
- **mode**: `ToolMode::Server`

**Schema simplification rules** (important for model performance):
- Resolve all `$ref` references inline
- Collapse `allOf` with single item to just that item
- Remove `readOnly` properties from request schemas
- Remove `writeOnly` properties from response schemas
- Strip `example`, `default`, `x-*` extensions
- Cap `enum` values at 20 (truncate with "... and N more")
- If a schema exceeds 50 properties, split into required-only and provide the rest via description text

### 4. MCP over HTTP

Detect MCP by attempting Streamable HTTP first (POST with `mcp-protocol-version` header), then SSE (GET expecting `text/event-stream`).

On detection:
- Perform initialize handshake
- Call `tools/list`
- Generate `ToolSpec` entries like `phyl-tool-mcp` does (prefixed with `{name}_`)
- Route tool calls through MCP `tools/call` over the same transport

### 5. Auth: internal handling with human escalation

**Key principle:** `phyl-tool-api` handles auth internally. The agent never touches raw credentials. When human action is needed, the tool returns a structured message and the agent passes it along via `ask_human`.

#### What `api_connect` does automatically (no agent involvement):

1. Checks `$PHYLACTERY_HOME/tokens/{name}.json` for OAuth tokens
2. Checks `secrets.env` for `{NAME}_API_KEY`, `{NAME}_TOKEN`, `{NAME}_BEARER_TOKEN`
3. Auto-refreshes expired OAuth tokens using stored refresh tokens
4. Fetches `.well-known/openid-configuration` or `.well-known/oauth-authorization-server` to discover OAuth endpoints
5. Reads `securitySchemes` from OpenAPI spec
6. Applies credentials to a test request to verify they work

The agent only gets involved when credentials are missing or invalid.

#### OAuth flows (handled by `api_auth`)

**Authorization Code + PKCE:**
- `api_auth` starts a localhost callback server on a random port
- Constructs the full authorization URL
- Returns it in the response (the agent sends it to the human via `ask_human`)
- Blocks on this request ID while listening for the callback
- On callback: exchanges code for tokens, stores them, returns success
- On timeout (5 min): returns error suggesting device flow as alternative

**Device Code:**
- `api_auth` requests a device code from the provider
- Returns the verification URL + user code (agent sends to human)
- Polls token endpoint internally every 5 seconds
- On success: stores tokens, returns success
- On timeout/denial: returns error

**Client ID problem:** Most OAuth providers require a registered application.
- For well-known providers (GitHub, Google, Slack, Microsoft, etc.), ship a `$PHYLACTERY_HOME/oauth_clients.toml` with pre-registered client IDs (phylactery project registers as an OAuth app with each). Override via config.
- For unknown providers, `api_auth` returns `"need_client_id": true` — the agent asks the human to register an OAuth app and provide the client ID/secret. Store them in `secrets.env` as `{NAME}_OAUTH_CLIENT_ID` / `{NAME}_OAUTH_CLIENT_SECRET` for reuse.
- If the provider supports Device Code flow, prefer it — many providers allow device flow without a client secret.

#### API key / bearer token flow

When `api_connect` returns `auth.type == "api_key"` and `has_credentials == false`:

The response includes `instructions` — a human-readable string like:
> "This API requires an API key passed as header X-API-Key. The OpenAPI spec links to https://acme.com/developers for key management."

The agent calls `ask_human` with this info. The human pastes the key. The agent calls `secret_store(key="{NAME}_API_KEY", value=<response>)`. Then calls `api_connect` again — this time the credential is found and applied.

#### Token storage

- **OAuth tokens**: `$PHYLACTERY_HOME/tokens/{name}.json` — JSON with `access_token`, `refresh_token`, `expires_at`, `token_type`, `scopes`, `token_url` (for refresh).
- **API keys**: `$PHYLACTERY_HOME/secrets.env` — `{NAME}_API_KEY=value` or `{NAME}_TOKEN=value`.
- **OAuth client credentials**: `$PHYLACTERY_HOME/secrets.env` — `{NAME}_OAUTH_CLIENT_ID`, `{NAME}_OAUTH_CLIENT_SECRET`.
- All files `0600` permissions.
- `phyl-tool-api` reads these directly from disk. Credentials never appear in tool call arguments or `log.jsonl`.

### 6. Cookie jar and session state

Each registered API connection gets a `reqwest::Client` with a persistent `cookie_store`. This is critical for:

- **Post-CAPTCHA state**: When the human solves a CAPTCHA, the challenge provider typically sets a session cookie. But here's the problem: the human solves it in their browser, not in `phyl-tool-api`'s HTTP client.

**Realistic CAPTCHA handling:**

CAPTCHAs in the context of API access fall into two categories:

1. **API-level challenges** (Cloudflare "I'm Under Attack", bot detection on API endpoints): These return a challenge page instead of the API response. The human can't meaningfully solve these for `phyl-tool-api` because the cookie would be set in the human's browser, not the tool's HTTP client. **Mitigation**:
   - Return the raw challenge response to the agent with a `human_challenge` error
   - Include a `cookie_import` option: the agent asks the human to solve the challenge in their browser and then paste the `cf_clearance` (or equivalent) cookie value. `phyl-tool-api` injects it into the cookie jar. This is clunky but honest.
   - Better: if the API offers an API key or OAuth as an alternative to browser-based access (most do), guide the agent to use that instead. Bot challenges usually don't fire on authenticated API requests.

2. **OAuth consent-flow challenges** (CAPTCHA on the login page during OAuth): These happen in the human's browser during the OAuth flow. The human is already in the browser, so they naturally complete the CAPTCHA as part of signing in. No special handling needed — the OAuth callback still fires normally.

**Structured challenge response:**
```json
{
  "error": "human_challenge",
  "challenge_type": "bot_protection" | "captcha" | "mfa" | "email_verify",
  "message": "Cloudflare bot protection detected. This usually means the API requires authenticated access rather than anonymous requests.",
  "suggestions": [
    "Try authenticating with an API key instead of anonymous access",
    "Ask the human for a cf_clearance cookie from their browser"
  ],
  "raw_status": 403,
  "raw_body_snippet": "<!DOCTYPE html>... Checking your browser..."
}
```

The `suggestions` field gives the agent actionable next steps rather than a generic "ask the human." The agent can try the first suggestion (switch to authenticated access) before bothering the human.

### 7. Secrets management: `phyl-tool-secrets` (oneshot)

Separate crate. Single source of truth for `secrets.env`. Used by the agent for all credential storage — API keys, tokens, anything.

```
secret_check(key: string) → { "exists": bool }
secret_store(key: string, value: string) → { "stored": true }
secret_delete(key: string) → { "deleted": true }
secret_list() → { "keys": ["ACME_API_KEY", ...] }
```

`phyl-tool-api` reads `secrets.env` directly from disk (not through this tool) when it needs credentials. The tool is for the agent to manage secrets — store what the human provides, check what's available before asking.

### 8. Log redaction

**In `phyl-core`:** Add `redact_params` to `ToolSpec`:

```rust
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub mode: ToolMode,
    pub parameters: serde_json::Value,
    pub sandbox: Option<SandboxSpec>,
    pub redact_params: Option<Vec<String>>,  // param names to scrub from logs
}
```

**In `phyl-run`:** When writing tool calls to `log.jsonl`, replace redacted param values with `"[REDACTED]"` in the logged copy.

`secret_store` declares `redact_params: ["value"]`. So `log.jsonl` shows `secret_store(key="ACME_API_KEY", value="[REDACTED]")`.

### 9. Overlap with `phyl-tool-web`

`phyl-tool-web` provides `http_fetch`, `http_post`, `http_put`, `web_read`, `web_search`. `api_call` is similar but critically different:
- Uses the connection's auth (bearer token, API key, cookies)
- Uses the connection's cookie jar (session state persists)
- Uses the connection's base URL (paths are relative)
- Auto-refreshes OAuth tokens

No overlap — `phyl-tool-web` is for anonymous/ad-hoc web access. `api_call` is for authenticated API access through a registered connection. The agent uses `web_read` to read API docs, then `api_connect` + generated tools to actually use the API.

## Typical Agent Flows

### First-time OAuth API (e.g., GitHub)

```
Agent: "I need to check your GitHub pull requests."
  → api_connect(name="github", url="https://api.github.com")
  ← { status: "auth_required",
       auth: { type: "oauth2", has_credentials: false,
               oauth: { authorize_url: "...", token_url: "...",
                        device_authorize_url: "https://github.com/login/device/code",
                        scopes_available: ["repo","read:user",...] },
               instructions: "GitHub API requires OAuth. Device code flow available." }}
  → api_auth(name="github", method="oauth_device", scopes=["repo","read:user"])
  ← (tool blocks while polling GitHub's device endpoint)
  ← { "verification_url": "https://github.com/login/device",
       "user_code": "ABCD-1234",
       "message": "Waiting for user to authorize..." }
     (this is an intermediate response — agent sees it immediately)
  → ask_human("Please go to https://github.com/login/device and enter code ABCD-1234")
  ← (api_auth still blocking, polls every 5s, user completes auth)
  ← { "status": "authenticated", "tools_registered": 47 }
     (tools_changed signal emitted)
  → github_list_pulls(owner="user", repo="phylactery", state="open")
  ← { status: 200, body: [...] }
```

Note: only 1 human interaction (paste a code), and the agent figured out device flow was available on its own.

### Returning session (credentials stored)

```
Agent: "Check GitHub PRs again."
  → api_connect(name="github", url="https://api.github.com")
  ← { status: "connected", tools_registered: 47 }
     (found tokens/github.json, auto-refreshed, probe succeeded)
  → github_list_pulls(...)
```

Zero human interaction.

### Simple API key (e.g., OpenWeatherMap)

```
Agent: "What's the weather in NYC?"
  → api_connect(name="weather", url="https://api.openweathermap.org")
  ← { status: "auth_required",
       auth: { type: "api_key", has_credentials: false,
               instructions: "Requires API key as query param 'appid'. Get one at https://openweathermap.org/appkeys" }}
  → ask_human("I need an OpenWeatherMap API key. You can get one at https://openweathermap.org/appkeys — please paste it here.")
  ← (human pastes key)
  → secret_store(key="WEATHER_API_KEY", value=<human's response>)
  → api_connect(name="weather", url="https://api.openweathermap.org")
  ← { status: "connected", tools_registered: 8 }
  → weather_get_current(q="New York,US")
  ← { status: 200, body: { temp: 42, ... } }
```

### API without OpenAPI spec

```
Agent: "Monitor my Hacker News karma."
  → api_connect(name="hn", url="https://hacker-news.firebaseio.com/v0")
  ← { status: "no_spec", auth: { type: "none" },
       spec_format: "none" }
  → web_read(url="https://github.com/HackerNews/API")
  ← (reads the API docs, understands the endpoints)
  → api_register_tools(name="hn", tools=[
      { tool_name: "get_user", method: "GET", path: "/user/{id}.json",
        description: "Get user profile by username",
        query_params: {}, body_schema: null },
      { tool_name: "get_item", method: "GET", path: "/item/{id}.json",
        description: "Get item (story, comment, etc.) by ID",
        query_params: {}, body_schema: null }
    ])
  ← { tools_registered: 2 }
  → hn_get_user(id="dang")
  ← { status: 200, body: { karma: 12345, ... } }
```

### Bot protection encountered

```
Agent: (calling a registered API endpoint)
  → acme_list_products(category="widgets")
  ← { error: "human_challenge", challenge_type: "bot_protection",
       message: "Cloudflare bot protection detected...",
       suggestions: ["Try authenticating with an API key instead of anonymous access",
                     "Ask the human for a cf_clearance cookie from their browser"] }
  → (agent tries suggestion 1 first)
  → ask_human("The Acme API is blocking automated requests. Do you have an API key? Check https://acme.com/developers")
  ← (human provides key)
  → secret_store(key="ACME_API_KEY", value=<key>)
  → api_connect(name="acme", url="https://api.acme.com")
  ← { status: "connected", ... }
  → acme_list_products(category="widgets")
  ← { status: 200, body: [...] }
```

## Implementation Steps

1. **`phyl-core` changes** — add `tools: Option<Vec<ToolSpec>>` to `ServerResponse`, add `redact_params: Option<Vec<String>>` to `ToolSpec`
2. **`phyl-run` changes** — handle `tools_changed` signal (update `all_specs` + `tool_map`), add log redaction for `redact_params`
3. **`phyl-tool-secrets`** — new oneshot crate: `secret_check`, `secret_store`, `secret_delete`, `secret_list` against `secrets.env`
4. **`phyl-tool-api` skeleton** — new server-mode crate with `api_connect`, `api_call`, `api_disconnect`, `api_register_tools`, `api_auth` tool specs
5. **Spec auto-discovery** — probe common paths, parse OpenAPI 3.x / Swagger 2.x / HTML
6. **OpenAPI → ToolSpec generation** — schema flattening, `$ref` resolution, name generation
7. **Dynamic endpoint dispatch** — route generated tool calls to HTTP requests with auth + cookies
8. **Auth detection** — parse `securitySchemes`, `.well-known` discovery, credential lookup from `secrets.env` + `tokens/`
9. **OAuth Authorization Code + PKCE** — localhost callback server, token exchange, blocking response with multiplexing
10. **OAuth Device Code** — device code request, polling, blocking response
11. **Token persistence + auto-refresh** — `tokens/{name}.json`, refresh before expiry, re-auth on refresh failure
12. **`api_register_tools`** — manual tool definition for spec-less APIs
13. **Challenge detection** — CAPTCHA/bot protection heuristics, structured error responses with suggestions
14. **MCP over HTTP** — Streamable HTTP + SSE transport detection and bridging
15. **Pre-registered OAuth clients** — `oauth_clients.toml` with client IDs for common providers
16. **Tests** — unit tests for spec parsing, schema flattening, auth flows (mocked), secrets CRUD, challenge detection; integration tests for connect → auth → call flows

## What This Doesn't Change

- `phyl-tool-mcp` — untouched, continues handling stdio MCP from config.toml
- `phyl-tool-web` — untouched, remains the anonymous/ad-hoc web access tool
- Tool discovery — `phyl-tool-api` is found via `phyl-tool-*` PATH pattern like everything else
- Model adapter protocol — no changes (`ModelRequest.tools` is already `Vec<ToolSpec>`)

## Decided

- **Rate limiting**: `phyl-tool-api` retries 429s with `Retry-After` up to 3 times. Passes `X-RateLimit-*` headers through in output so the agent can pace itself.
- **Pagination**: Exposed as regular tool parameters from the spec. No auto-pagination. Response includes pagination metadata (`next_cursor`, `has_more`, link headers).
- **CAPTCHAs**: Returned as structured `human_challenge` errors with actionable `suggestions`. Agent tries non-human solutions first (switch to authenticated access) before escalating.
- **Schema complexity**: Flatten aggressively. Resolve `$ref`s, collapse trivial `allOf`, strip examples/extensions, cap enum values. 50-property limit per tool.
- **Non-OpenAPI APIs**: Agent reads docs via `web_read`, then calls `api_register_tools` to define endpoints manually. `api_call` available as immediate fallback.
- **OAuth client IDs**: Ship pre-registered clients for major providers. For others, agent asks human to register an app and stores the client ID/secret in `secrets.env`.

## Open Questions

- **Server-mode multiplexing**: v1 keeps sequential dispatch with a "don't call other tools while `api_auth` blocks" constraint. Real multiplexing (threaded response readers per server tool) is a future `phyl-run` improvement. Is the v1 constraint acceptable or should multiplexing be a prerequisite?
- **Secret rotation**: Is `secret_store` (overwrite) sufficient, or do we need explicit rotation workflows?
- **Connection persistence across sessions**: Should `api_connect` state (registered endpoints, cookie jars) be serializable to disk so a new session can restore a connection without re-parsing the spec? Credentials already persist; this would persist the tool definitions too.
