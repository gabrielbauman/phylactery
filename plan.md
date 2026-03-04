# Dynamic API Tool Discovery

## Goal

Let the agent discover and use arbitrary remote APIs from within a session — no user configuration needed. Given a URL (API endpoint, docs page, or OpenAPI spec), the agent reads the API description, generates tool definitions, handles auth, and makes the tools callable for the rest of the session.

## Design Overview

Two new capabilities working together:

1. **`api_connect` tool** — A new server-mode tool (`phyl-tool-api`) that the agent calls to register an API and invoke its endpoints at runtime.
2. **Dynamic tool injection in `phyl-run`** — A new signal type (`"tools_changed"`) that lets server-mode tools report new tool definitions mid-session, which `phyl-run` adds to subsequent model requests.

The agent flow looks like:
```
Agent: "I need to use the Acme API."
       → calls api_connect(url="https://api.acme.com/openapi.json",
                           auth={"type": "bearer", "token": "sk-..."})
       → phyl-tool-api fetches the spec, parses it, generates ToolSpecs
       → returns new tool list to phyl-run via tools_changed signal
       → phyl-run adds tools to all_specs for next model invocation
Agent: "Now let me list users."
       → calls acme_list_users(...)
       → phyl-run routes to phyl-tool-api (server-mode)
       → phyl-tool-api makes the actual HTTP request, returns result
```

## Component Changes

### 1. New crate: `phyl-tool-api`

A server-mode tool providing these tools:

#### `api_connect` — Register an API
```
Parameters:
  url: string        — URL to an OpenAPI/Swagger spec, API docs page, or base URL
  name: string       — Prefix for generated tool names (e.g., "acme")
  auth:              — Optional auth configuration
    type: "bearer" | "basic" | "header" | "query"
    token: string    — For bearer auth
    username: string — For basic auth
    password: string — For basic auth
    header_name: string  — For custom header auth (e.g., "X-API-Key")
    header_value: string
    param_name: string   — For query param auth
    param_value: string
  headers: object    — Optional extra headers to include on all requests
```

**Behavior:**
1. Fetch the URL
2. Detect format:
   - **OpenAPI 3.x / Swagger 2.x JSON/YAML**: Parse spec directly
   - **HTML page**: Extract API docs, use heuristics or return content for the model to interpret
   - **MCP endpoint**: Detect MCP protocol, perform initialize + tools/list handshake
3. Generate `ToolSpec` entries, each prefixed with `{name}_` (e.g., `acme_list_users`)
4. Store the API config (base URL, auth, endpoint details) in internal state
5. Return the tool list as output AND emit a `tools_changed` signal with the new specs

#### `api_disconnect` — Remove a registered API
```
Parameters:
  name: string — The API prefix to remove
```
Removes the API from internal state, emits `tools_changed` with updated full tool list.

#### Dynamic endpoint tools — Generated per-API
Each discovered endpoint becomes a callable tool routed through `phyl-tool-api`. When the tool is called:
1. Look up the endpoint definition in internal state
2. Build the HTTP request (method, URL, path params, query params, body)
3. Apply auth configuration
4. Make the HTTP request
5. Return response body (with status code in output)

#### `api_call` — Fallback raw HTTP tool
```
Parameters:
  name: string       — Registered API name (for auth/base URL)
  method: string     — GET, POST, PUT, DELETE, PATCH
  path: string       — Path relative to base URL
  query: object      — Query parameters
  body: object       — Request body (JSON)
  headers: object    — Additional headers
```
For when the model wants to call an endpoint not in the discovered spec, or for APIs without formal specs.

### 2. Protocol extension: `tools_changed` signal

**In `phyl-core`:**

Extend `ServerResponse` with an optional `tools` field:

```rust
pub struct ServerResponse {
    pub id: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub signal: Option<String>,           // existing
    pub tools: Option<Vec<ToolSpec>>,      // NEW — present when signal == "tools_changed"
}
```

When `signal == "tools_changed"`, `tools` contains the **complete** list of tools that this server-mode tool process now provides. This is a full replacement, not a diff — simpler and avoids state sync bugs.

**In `phyl-run`:**

After dispatching a server-mode tool call, check for `tools_changed` signal:

```rust
if response.signal.as_deref() == Some("tools_changed") {
    if let Some(new_tools) = response.tools {
        // 1. Remove all existing specs from this server tool executable
        all_specs.retain(|s| tool_map.get(&s.name).map(|ti| &ti.executable) != Some(&exec_path));
        // 2. Add the new specs
        all_specs.extend(new_tools.clone());
        // 3. Update tool_map with new entries
        for spec in &new_tools {
            tool_map.insert(spec.name.clone(), ToolInfo {
                executable: exec_path.clone(),
                mode: spec.mode.clone(),
            });
        }
    }
}
```

This means `all_specs` and `tool_map` must become mutable in the agentic loop (they currently aren't reassigned but this is a small change).

### 3. OpenAPI spec parsing

Add `phyl-tool-api` dependency on an OpenAPI parser. Approach:

- Use the `utoipa` or `openapiv3` crate to parse OpenAPI 3.x specs
- For Swagger 2.x, convert to 3.x or parse directly
- For each endpoint, generate a `ToolSpec`:
  - `name`: `{api_name}_{operation_id}` or `{api_name}_{method}_{path_slug}`
  - `description`: From the endpoint's summary/description
  - `parameters`: JSON Schema derived from the endpoint's parameters + request body
  - `mode`: `ToolMode::Server` (all routed through `phyl-tool-api`)

### 4. MCP over HTTP (bonus, same tool)

Since MCP is just another API protocol, `api_connect` can detect MCP servers (SSE or Streamable HTTP transport) and handle them too:

- Detect via URL path conventions or initial response headers
- Perform MCP initialize handshake over HTTP
- Call `tools/list` to discover tools
- Route subsequent tool calls through MCP `tools/call` over HTTP

This means **no changes needed to `phyl-tool-mcp`** — it continues handling stdio-based MCP servers from config.toml. HTTP-based MCP servers go through `phyl-tool-api`.

### 5. Auth: OAuth flows, interactive token prompts, and persistent storage

The agent needs to handle auth end-to-end — from discovering what auth an API requires, to acquiring credentials (interactively if needed), to storing and refreshing them across sessions.

#### Auth acquisition strategies (in order of preference)

1. **Pre-stored secrets** — Check `$PHYLACTERY_HOME/secrets.env` first. If a key like `ACME_API_KEY` already exists, use it immediately. The agent can reference these via `$VAR` expansion (existing pattern from `phyl-tool-mcp`).

2. **OAuth 2.0 Authorization Code flow** — For APIs that support OAuth:
   - Agent calls `api_auth_oauth` with the provider's authorization URL, token URL, client ID, and scopes
   - `phyl-tool-api` starts a temporary local HTTP server on a random port (e.g., `http://localhost:48271/callback`) to receive the redirect
   - Constructs the authorization URL with `redirect_uri`, `state`, PKCE `code_challenge`
   - Returns the URL to the agent as tool output
   - Agent calls `ask_human` (existing tool) to tell the user: "Please visit this URL to authorize access: https://accounts.acme.com/oauth/authorize?..."
   - User clicks link, authenticates, gets redirected to localhost callback
   - `phyl-tool-api` receives the auth code, exchanges it for access + refresh tokens
   - Stores tokens persistently (see storage below)
   - Returns success to agent

3. **OAuth 2.0 Device Code flow** — For headless environments where localhost redirect isn't possible:
   - Agent calls `api_auth_device` with device authorization URL, token URL, client ID
   - `phyl-tool-api` requests a device code from the provider
   - Returns the user code and verification URL
   - Agent calls `ask_human`: "Please visit https://acme.com/device and enter code: ABCD-1234"
   - `phyl-tool-api` polls the token endpoint until the user completes auth
   - Stores tokens, returns success

4. **Interactive token prompt** — For APIs using simple API keys or tokens:
   - Agent determines the API needs a key (from OpenAPI `securitySchemes`, or from a 401 response)
   - Agent calls `ask_human`: "The Acme API requires an API key. You can get one at https://acme.com/settings/api. Please paste it here."
   - User pastes the token via whatever bridge they use (terminal, Signal, etc.)
   - Agent calls `api_store_secret` to persist it

5. **No auth** — Some APIs are public. Just connect and go.

#### Auth tools provided by `phyl-tool-api`

```
api_auth_oauth — Start OAuth 2.0 Authorization Code + PKCE flow
  Parameters:
    name: string              — API name (used as storage key prefix)
    authorize_url: string     — Provider's authorization endpoint
    token_url: string         — Provider's token endpoint
    client_id: string         — OAuth client ID
    scopes: string[]          — Requested scopes
    client_secret: string     — Optional (some providers require it)

api_auth_device — Start OAuth 2.0 Device Code flow
  Parameters:
    name: string              — API name
    device_authorize_url: string — Device authorization endpoint
    token_url: string         — Token endpoint
    client_id: string         — OAuth client ID
    scopes: string[]          — Requested scopes

api_store_secret — Persist a credential
  Parameters:
    key: string               — Secret key name (e.g., "ACME_API_KEY")
    value: string             — Secret value

api_check_secret — Check if a credential exists (without revealing it)
  Parameters:
    key: string               — Secret key name
  Returns: { "exists": true/false }
```

#### Token storage and refresh

**Storage location:** `$PHYLACTERY_HOME/secrets.env` for simple API keys (same file the CLI already manages via `phyl config add-secret`), and `$PHYLACTERY_HOME/tokens/{name}.json` for OAuth tokens that include refresh tokens and expiry.

**Token file format** (`tokens/acme.json`):
```json
{
  "access_token": "eyJ...",
  "refresh_token": "dGhp...",
  "token_type": "bearer",
  "expires_at": "2026-03-04T15:30:00Z",
  "scopes": ["read", "write"]
}
```

**Auto-refresh:** When `phyl-tool-api` makes an HTTP request and the stored token is expired (or within 60s of expiry), it automatically uses the refresh token to get a new access token before making the request. If refresh fails (revoked, expired refresh token), it returns an error telling the agent to re-authenticate.

**Cross-session persistence:** Since tokens live in `$PHYLACTERY_HOME` (not the session directory), any future session can use previously acquired credentials. The agent just calls `api_connect(name="acme", url="...")` and `phyl-tool-api` finds existing tokens automatically.

**Security:**
- Token files are created with `0600` permissions (owner-only read/write)
- `secrets.env` already exists with this pattern
- Tokens never appear in `log.jsonl` — `phyl-tool-api` reads them from disk internally, the agent never sees the raw values
- `api_store_secret` writes directly to `secrets.env`; the secret value passes through one tool call but is not echoed back

#### Typical agent flows

**OAuth API (e.g., GitHub, Google):**
```
Agent: I need to access the GitHub API.
  → api_check_secret(key="GITHUB_OAUTH_ACCESS") → { exists: false }
  → api_auth_oauth(name="github", authorize_url="https://github.com/login/oauth/authorize",
                   token_url="https://github.com/login/oauth/access_token",
                   client_id="...", scopes=["repo", "read:user"])
  → Returns: "Authorization URL: https://github.com/login/oauth/authorize?..."
  → ask_human("Please visit this URL to authorize GitHub access: https://github.com/...")
  → User clicks, authorizes, callback received
  → Tokens stored in $PHYLACTERY_HOME/tokens/github.json
  → api_connect(name="github", url="https://api.github.com", auth={type: "stored", name: "github"})
  → Tools discovered, agent proceeds
```

**Simple API key (e.g., OpenWeatherMap):**
```
Agent: I need weather data from OpenWeatherMap.
  → api_connect(name="weather", url="https://api.openweathermap.org/...")
  → Gets 401, spec shows apiKey security scheme
  → api_check_secret(key="OPENWEATHER_API_KEY") → { exists: false }
  → ask_human("The OpenWeatherMap API requires an API key. Get one at
               https://openweathermap.org/api — paste it here.")
  → User pastes: "abc123def456"
  → api_store_secret(key="OPENWEATHER_API_KEY", value="abc123def456")
  → api_connect(name="weather", url="...", auth={type: "query", param_name: "appid", value: "$OPENWEATHER_API_KEY"})
  → Connected, tools available
```

**Returning session (tokens already stored):**
```
Agent: Let me check your GitHub notifications.
  → api_check_secret(key="GITHUB_OAUTH_ACCESS") → { exists: true }
     (or just: api_connect detects tokens/github.json exists)
  → api_connect(name="github", url="https://api.github.com", auth={type: "stored", name: "github"})
  → Access token auto-refreshed if expired
  → Tools available immediately, no user interaction needed
```

**CAPTCHA or human-verification challenge:**
```
Agent: Fetching data from acme.com...
  → api_call or endpoint tool returns a response indicating a CAPTCHA/challenge
     (detected via: HTTP 403/429 with challenge HTML, known CAPTCHA provider signatures
      like reCAPTCHA/hCaptcha/Cloudflare Turnstile in response body, or explicit
      challenge JSON fields)
  → phyl-tool-api returns structured error:
     { "error": "human_challenge", "challenge_type": "captcha",
       "url": "https://acme.com/verify?token=abc", "message": "CAPTCHA required" }
  → Agent calls ask_human("I hit a CAPTCHA while accessing acme.com.
     Please visit this URL and complete the verification: https://acme.com/verify?token=abc
     Let me know when you're done.")
  → User completes the challenge, replies "done"
  → Agent retries the original request (session cookies/tokens may now be valid)
```

#### Human-in-the-loop challenge handling

Some auth and access flows will hit automation blockers that only a human can solve. `phyl-tool-api` should detect these and return structured errors the agent can act on:

**Detection heuristics:**
- HTTP 403/429 responses containing CAPTCHA provider markers (`recaptcha`, `hcaptcha`, `cf-challenge`, `turnstile`)
- Response bodies with `<form>` elements pointing to known challenge endpoints
- OAuth flows that redirect to consent screens requiring manual interaction beyond the standard authorize page
- SMS/email MFA prompts during token exchange
- "Verify you're human" interstitials (Cloudflare, Akamai, etc.)

**Structured challenge response format:**
```json
{
  "error": "human_challenge",
  "challenge_type": "captcha" | "mfa" | "consent" | "email_verify" | "unknown",
  "url": "https://...",            // URL for the user to visit (if applicable)
  "message": "Human-readable description of what's needed",
  "retry_after": null              // Optional: seconds to wait before retrying
}
```

**Agent behavior pattern:** When `phyl-tool-api` returns a `human_challenge` error, the agent should:
1. Call `ask_human` with the URL and clear instructions
2. Wait for the human's response
3. Retry the original request
4. If it fails again, escalate with more detail rather than retrying in a loop

### 6. General secrets management tool

Rather than limiting secret management to API auth, give the agent a general-purpose secrets tool. This is useful beyond API connections — the agent might need to store SSH keys, database passwords, webhook secrets, etc.

**New crate: `phyl-tool-secrets`** (oneshot tool)

```
secret_check — Check if a secret exists
  Parameters:
    key: string
  Returns: { "exists": true, "key": "ACME_API_KEY" }

secret_store — Store a secret
  Parameters:
    key: string        — Key name (uppercase, underscores recommended)
    value: string      — Secret value
  Returns: { "stored": true, "key": "ACME_API_KEY" }

secret_delete — Remove a secret
  Parameters:
    key: string
  Returns: { "deleted": true, "key": "ACME_API_KEY" }

secret_list — List all secret keys (values never exposed)
  Returns: { "keys": ["ACME_API_KEY", "GITHUB_TOKEN", ...] }
```

**Storage:** Reads/writes `$PHYLACTERY_HOME/secrets.env` (same file as `phyl config add-secret`). This means secrets added by the agent are visible to `phyl config list-secrets` and vice versa — single source of truth.

**Security considerations:**
- Values written to `secrets.env` with `0600` permissions
- The `secret_store` call will contain the raw value in the tool call arguments, which means it appears in `log.jsonl`. To mitigate: `phyl-run` should redact tool arguments for `secret_store` calls in the log (replace `value` with `"[REDACTED]"`). Add a `redact_args` field to `ToolSpec` listing parameter names to scrub from logs.
- `secret_list` never returns values, only keys
- `secret_check` returns boolean, not the value
- Tools that consume secrets (like `phyl-tool-api`, `phyl-tool-mcp`) read them from disk via `$VAR` expansion — the agent never sees the resolved value

**`phyl-tool-api` integration:** When `api_connect` receives `auth.token = "$ACME_API_KEY"`, `phyl-tool-api` expands it from `secrets.env` + environment. The agent's workflow becomes:

```
Agent needs an API key →
  secret_check("ACME_API_KEY") → not found →
  ask_human("Please provide your Acme API key") →
  secret_store("ACME_API_KEY", <user's response>) →
  api_connect(name="acme", url="...", auth={type:"bearer", token:"$ACME_API_KEY"})
```

The raw token value only exists briefly in the `secret_store` call — after that, everything uses the `$VAR` reference.

### 7. Log redaction for secrets

**In `phyl-core`:** Add optional `redact_params` to `ToolSpec`:

```rust
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub mode: ToolMode,
    pub parameters: serde_json::Value,
    pub sandbox: Option<SandboxSpec>,
    pub redact_params: Option<Vec<String>>,  // NEW — param names to scrub from logs
}
```

**In `phyl-run`:** When writing a tool call to `log.jsonl`, check if the tool's spec has `redact_params`. If so, replace those argument values with `"[REDACTED]"` in the logged copy (not the actual call).

This keeps `log.jsonl` safe to share/review while still allowing the tool to receive the real values.

## Implementation Steps

1. **Add `tools` field to `ServerResponse`** in `phyl-core` (1 line + serde attrs)
2. **Add `redact_params` field to `ToolSpec`** in `phyl-core`
3. **Handle `tools_changed` signal in `phyl-run`** — update `all_specs` and `tool_map` when received (~20 lines)
4. **Add log redaction in `phyl-run`** — scrub `redact_params` fields when writing tool calls to `log.jsonl`
5. **Create `phyl-tool-secrets` crate** — oneshot tool for secret_check, secret_store, secret_delete, secret_list against `secrets.env`
6. **Create `phyl-tool-api` crate** with Cargo.toml and basic structure
7. **Implement `api_connect` with OpenAPI parsing** — fetch URL, parse spec, generate ToolSpecs, return via `tools_changed`
8. **Implement dynamic endpoint dispatch** — receive `ServerRequest` for generated tool names, make HTTP calls, return results
9. **Implement `api_call` fallback** — raw HTTP tool for unstructured API access
10. **Implement auth handling** — bearer, basic, header, query param, and `stored` (OAuth tokens) auth with env var expansion
11. **Implement OAuth Authorization Code + PKCE flow** — local callback server, token exchange, persistent token storage in `$PHYLACTERY_HOME/tokens/`
12. **Implement OAuth Device Code flow** — for headless environments
13. **Implement token auto-refresh** — transparent refresh before expired tokens are used
14. **Add MCP-over-HTTP detection** (optional, can defer) — detect and handle HTTP-based MCP servers
15. **Tests** — unit tests for OpenAPI parsing, OAuth flows (mocked), secrets management, log redaction, integration tests for the full flow

## What This Doesn't Change

- `phyl-tool-mcp` — untouched, continues handling stdio MCP servers from config
- Tool discovery — `phyl-tool-api` is discovered normally via `phyl-tool-*` pattern on PATH
- Oneshot tools — no changes
- Model adapter protocol — no changes (tools are already a `Vec<ToolSpec>` in `ModelRequest`)

## Decided

- **Rate limiting**: Yes. `phyl-tool-api` respects `429` responses and `Retry-After` headers. On 429, it waits the indicated duration (or a default backoff) and retries automatically up to 3 times. If still rate-limited, it returns the error to the agent with the retry-after duration so the agent can decide whether to wait or move on. Response headers like `X-RateLimit-Remaining` and `X-RateLimit-Reset` are included in tool output metadata so the agent can pace itself proactively.
- **Pagination**: Pagination parameters are part of the generated tool's calling interface — `page`, `cursor`, `offset`/`limit`, `next_token`, etc. are exposed as regular tool parameters derived from the OpenAPI spec. The tool does NOT auto-paginate. It's up to the agent to request subsequent pages. Pagination metadata from the response (`next_cursor`, `has_more`, `total_count`, link headers) is included in the tool output so the agent knows when to stop.
- **CAPTCHAs and human challenges**: Detected and routed to the human via `ask_human`. See "Human-in-the-loop challenge handling" section above.

## Open Questions

- **Schema complexity**: Should we flatten deeply nested OpenAPI schemas into simpler tool parameters, or pass them through? Simpler schemas = better model performance.
- **Non-OpenAPI APIs**: For APIs with only HTML docs (no machine-readable spec), should the tool return the docs as text and let the model figure out the endpoints, then use `api_call`? This seems like the pragmatic approach.
- **OAuth client IDs**: Some OAuth providers require registered client IDs. Should phylactery ship with pre-registered client IDs for common providers (GitHub, Google, etc.)? Or should the agent always ask the user to create their own OAuth app? A middle ground: ship defaults for popular providers but let the user override.
- **Secret rotation**: Should there be a `secret_rotate` tool or workflow for updating expired/compromised credentials? Or is `secret_store` (overwrite) sufficient?
- **Multi-user**: If phylactery ever supports multiple users, secrets would need per-user scoping. For now, single-user is fine.
