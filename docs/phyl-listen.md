# phyl-listen -- Event Listener

The push complement to `phyl-poll`. Where polling pulls data on intervals, listening receives events pushed by external systems. One binary, three listener types: webhooks, SSE subscriptions, and file watches.

## Why a Separate Binary?

The daemon deliberately listens only on a Unix socket -- no network exposure by design. A webhook receiver needs a TCP port. Putting this in the daemon would change its security model. A separate binary can crash/restart independently and can be omitted entirely if not needed.

## Webhooks

Receive HTTP POST requests from external services (GitHub, GitLab, CI/CD, monitoring, etc.).

```toml
[listen]
bind = "127.0.0.1:7890"

[[listen.hook]]
name = "github"
path = "/hook/github"
secret = "$GITHUB_WEBHOOK_SECRET"
prompt = "A GitHub event arrived."
route_header = "X-GitHub-Event"
routes = {
  push = "Code was pushed. Review the diff.",
  pull_request = "A PR was opened or updated. Review it.",
  issues = "A new issue was filed. Triage it."
}
rate_limit = 10
dedup_header = "X-GitHub-Delivery"
```

### Features

- **HMAC-SHA256 verification** for GitHub (`X-Hub-Signature-256`), GitLab (`X-Gitlab-Token`), and generic webhooks
- **Event-type routing**: map header values to event-specific prompts via `route_header` + `routes`
- **Header-based filtering**: `filter_header` + `filter_values` for matching specific event types
- **Rate limiting**: sliding window per hook (default 10/minute), returns 429 when exceeded
- **Deduplication**: 5-minute cache by delivery ID header, prevents duplicate sessions from webhook retries
- **Payload size limit**: configurable per hook (default 1 MB)
- **Multiple hooks per path**: matched in config order (first match wins), each with independent settings

### Response Codes

| Code | Meaning |
|------|---------|
| 202 | Accepted, session created |
| 401 | Invalid or missing webhook signature |
| 404 | No hooks configured for this path |
| 429 | Rate limit exceeded |

## SSE Subscriptions

Subscribe to external Server-Sent Events streams and create sessions when events arrive.

```toml
[[listen.sse]]
name = "deploy-events"
url = "https://internal.example.com/events/stream"
prompt = "A deployment event occurred."
headers = { Authorization = "Bearer $DEPLOY_API_TOKEN" }
events = ["deploy_start", "deploy_fail", "deploy_success"]
route_event = true
routes = {
  deploy_fail = "A deployment failed. Investigate immediately.",
  deploy_success = "A deployment succeeded. Verify health."
}
rate_limit = 5
```

### Features

- **Persistent connections** with automatic reconnection (exponential backoff, 5s to 60s)
- **`Last-Event-ID`** sent on reconnect (standard SSE resume protocol)
- **Event filtering**: only process specific event types via `events` list
- **Event routing**: route to different prompts based on SSE event type
- **Stale connection detection**: recycles connections with no activity for 5 minutes
- **Custom headers**: with environment variable expansion for auth tokens

## File Watches

Watch files or directories for changes using inotify and create sessions when matching events occur. Turn a directory into an inbox.

```toml
[[listen.watch]]
name = "inbox"
path = "/home/user/agent-inbox/"
recursive = true
events = ["create"]
glob = "*.eml"
debounce = 2
prompt = "A new file appeared in the inbox. Read it and process the contents."
```

### Features

- **inotify-based** for efficient kernel-level monitoring (Linux)
- **Recursive watching** with auto-add for new subdirectories
- **Event types**: `create`, `modify`, `delete`, `move_to`, `move_from`
- **Glob filtering**: only trigger on matching filenames
- **Debouncing**: coalesce rapid changes (editors often create multiple events per save)
- **Content inclusion**: small files (< 100 KB) are included in the prompt on create/modify
- **Hidden file skipping**: dotfiles ignored unless glob explicitly matches them

## Shared Infrastructure

All three listener types share:

- **Daemon client**: Unix socket HTTP client for `POST /sessions`
- **Rate limiting**: in-memory sliding window per source
- **Deduplication cache**: in-memory with 5-minute TTL
- **Graceful shutdown**: Ctrl-C stops all listeners

If no `[[listen.hook]]` sections exist, the HTTP server is not started (no port opened). SSE and watch listeners run regardless.

## Security Model

- Default bind is `127.0.0.1` (localhost only)
- Only configured paths accept requests
- HMAC-SHA256 verification for authenticated sources
- Rate limiting prevents session floods
- Payload size limits prevent abuse
