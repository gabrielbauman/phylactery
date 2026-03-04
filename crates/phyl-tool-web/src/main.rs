use phyl_core::{SandboxSpec, ToolInput, ToolMode, ToolOutput, ToolSpec};
use serde_json::Map;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Default fetch timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug)]
enum Browser {
    Chrome(PathBuf),
    Firefox(PathBuf),
}

fn tool_specs() -> Vec<ToolSpec> {
    let browser_sandbox = Some(SandboxSpec {
        paths_rw: vec!["/tmp".to_string()],
        paths_ro: vec![
            "/usr".to_string(),
            "/lib".to_string(),
            "/bin".to_string(),
            "/etc".to_string(),
            "/Applications".to_string(),
        ],
        net: true,
        max_cpu_seconds: Some(60),
        max_file_bytes: Some(104_857_600),
        max_procs: Some(64),
        max_fds: Some(256),
    });

    let net_sandbox = Some(SandboxSpec {
        paths_rw: vec![],
        paths_ro: vec![],
        net: true,
        max_cpu_seconds: Some(30),
        max_file_bytes: None,
        max_procs: None,
        max_fds: None,
    });

    let headers_schema = serde_json::json!({
        "type": "object",
        "description": "Optional HTTP headers as key-value pairs",
        "additionalProperties": { "type": "string" }
    });

    vec![
        ToolSpec {
            name: "http_fetch".to_string(),
            description: "Fetch a URL and return the raw response body. \
                          Use for JSON APIs, raw HTML inspection, or when you need the exact \
                          response bytes. For reading web page content, prefer web_read instead."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch (must be http:// or https://)"
                    },
                    "headers": headers_schema
                },
                "required": ["url"]
            }),
            sandbox: net_sandbox.clone(),
        },
        ToolSpec {
            name: "http_post".to_string(),
            description: "Send an HTTP POST request. Returns status, response headers, \
                          and body. Use for submitting data to APIs, webhooks, etc."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to POST to (must be http:// or https://)"
                    },
                    "body": {
                        "type": "string",
                        "description": "The request body"
                    },
                    "headers": headers_schema
                },
                "required": ["url"]
            }),
            sandbox: net_sandbox.clone(),
        },
        ToolSpec {
            name: "http_put".to_string(),
            description: "Send an HTTP PUT request. Returns status, response headers, \
                          and body. Use for updating resources via APIs."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to PUT to (must be http:// or https://)"
                    },
                    "body": {
                        "type": "string",
                        "description": "The request body"
                    },
                    "headers": headers_schema
                },
                "required": ["url"]
            }),
            sandbox: net_sandbox.clone(),
        },
        ToolSpec {
            name: "web_read".to_string(),
            description: "Fetch a URL and return the page content as clean, readable markdown. \
                          Strips scripts, styles, navigation, and other non-content elements. \
                          Preferred way to read web pages when you care about the text content \
                          rather than the DOM structure or raw HTML. Use for articles, \
                          documentation, and any page where you need to understand the content."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch (must be http:// or https://)"
                    },
                    "headers": headers_schema
                },
                "required": ["url"]
            }),
            sandbox: net_sandbox.clone(),
        },
        ToolSpec {
            name: "web_fetch".to_string(),
            description: "Fetch a URL using a headless browser with full JavaScript rendering. \
                          Use only when content is dynamically generated by JavaScript and \
                          web_read returns incomplete results. Heavy — requires Chrome, \
                          Chromium, or Firefox installed on the system."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch (must be http:// or https://)"
                    }
                },
                "required": ["url"]
            }),
            sandbox: browser_sandbox,
        },
        ToolSpec {
            name: "web_search".to_string(),
            description: "Search the web using DuckDuckGo and return results with titles, \
                          URLs, and snippets."
                .to_string(),
            mode: ToolMode::Oneshot,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                },
                "required": ["query"]
            }),
            sandbox: net_sandbox,
        },
    ]
}

/// Check if an executable exists on PATH via `which`, or at an absolute path.
fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        if path.exists() {
            return Some(path.to_path_buf());
        }
        return None;
    }
    // Use `which` to search PATH.
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

/// Detect the first available browser.
fn detect_browser() -> Option<Browser> {
    // Chrome / Chromium on PATH
    for name in [
        "google-chrome-stable",
        "google-chrome",
        "chromium-browser",
        "chromium",
    ] {
        if let Some(path) = find_executable(name) {
            return Some(Browser::Chrome(path));
        }
    }

    // Firefox on PATH
    if let Some(path) = find_executable("firefox") {
        return Some(Browser::Firefox(path));
    }

    // macOS application bundles
    let chrome_app = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    if let Some(path) = find_executable(chrome_app) {
        return Some(Browser::Chrome(path));
    }

    let chromium_app = "/Applications/Chromium.app/Contents/MacOS/Chromium";
    if let Some(path) = find_executable(chromium_app) {
        return Some(Browser::Chrome(path));
    }

    let firefox_app = "/Applications/Firefox.app/Contents/MacOS/firefox";
    if let Some(path) = find_executable(firefox_app) {
        return Some(Browser::Firefox(path));
    }

    None
}

/// Fetch a URL using headless Chrome/Chromium with --dump-dom.
fn fetch_with_chromium(browser_path: &Path, url: &str, timeout: Duration) -> ToolOutput {
    let user_data_dir = format!("/tmp/phyl-chrome-{}", std::process::id());

    let child = match Command::new(browser_path)
        .args([
            "--headless",
            "--dump-dom",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-software-rasterizer",
            "--disable-dev-shm-usage",
            &format!("--user-data-dir={user_data_dir}"),
            url,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return ToolOutput {
                output: None,
                error: Some(format!("Failed to spawn browser: {e}")),
            };
        }
    };

    let result = match wait_with_timeout(child, timeout) {
        Ok((_status, stdout, _stderr)) => {
            if stdout.is_empty() {
                ToolOutput {
                    output: None,
                    error: Some("Browser returned empty output".to_string()),
                }
            } else {
                ToolOutput {
                    output: Some(stdout),
                    error: None,
                }
            }
        }
        Err(e) => ToolOutput {
            output: None,
            error: Some(e),
        },
    };

    // Clean up user data dir.
    let _ = std::fs::remove_dir_all(&user_data_dir);

    result
}

/// Read a Marionette length-prefixed JSON message from a TCP stream.
fn marionette_recv(stream: &mut std::net::TcpStream) -> Result<serde_json::Value, String> {
    // Read length prefix terminated by ':'
    let mut len_buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match io::Read::read(stream, &mut byte) {
            Ok(0) => return Err("Connection closed while reading length prefix".to_string()),
            Ok(_) => {
                if byte[0] == b':' {
                    break;
                }
                len_buf.push(byte[0]);
            }
            Err(e) => return Err(format!("Failed to read from Marionette: {e}")),
        }
    }
    let len_str = String::from_utf8(len_buf).map_err(|e| format!("Invalid length prefix: {e}"))?;
    let len: usize = len_str
        .parse()
        .map_err(|e| format!("Invalid length value '{len_str}': {e}"))?;

    // Read exactly `len` bytes of JSON.
    let mut json_buf = vec![0u8; len];
    let mut read = 0;
    while read < len {
        match io::Read::read(stream, &mut json_buf[read..]) {
            Ok(0) => return Err("Connection closed while reading message body".to_string()),
            Ok(n) => read += n,
            Err(e) => return Err(format!("Failed to read message body: {e}")),
        }
    }

    serde_json::from_slice(&json_buf).map_err(|e| format!("Invalid JSON from Marionette: {e}"))
}

/// Send a Marionette command: [0, id, method, params]
fn marionette_send(
    stream: &mut std::net::TcpStream,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let msg = serde_json::json!([0, id, method, params]);
    let payload = serde_json::to_string(&msg).map_err(|e| format!("JSON serialize error: {e}"))?;
    let wire = format!("{}:{payload}", payload.len());
    stream
        .write_all(wire.as_bytes())
        .map_err(|e| format!("Failed to write to Marionette: {e}"))
}

/// Send a Marionette command and wait for the matching response [1, id, error, result].
fn marionette_call(
    stream: &mut std::net::TcpStream,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    marionette_send(stream, id, method, params)?;

    let resp = marionette_recv(stream)?;
    let arr = resp
        .as_array()
        .ok_or("Marionette response is not an array")?;

    if arr.len() < 4 {
        return Err(format!("Marionette response too short: {resp}"));
    }

    // arr[0] == 1 (response type), arr[1] == id, arr[2] == error, arr[3] == result
    if !arr[2].is_null() {
        return Err(format!("Marionette error: {}", arr[2]));
    }

    Ok(arr[3].clone())
}

/// Fetch a URL using headless Firefox via the Marionette protocol.
fn fetch_with_firefox(browser_path: &Path, url: &str, timeout: Duration) -> ToolOutput {
    // Find a free port.
    let port = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => {
                return ToolOutput {
                    output: None,
                    error: Some(format!("Failed to get local address: {e}")),
                };
            }
        },
        Err(e) => {
            return ToolOutput {
                output: None,
                error: Some(format!("Failed to bind for port discovery: {e}")),
            };
        }
    };

    // Create a temp profile directory.
    let profile_dir = format!("/tmp/phyl-firefox-{}", std::process::id());
    if let Err(e) = std::fs::create_dir_all(&profile_dir) {
        return ToolOutput {
            output: None,
            error: Some(format!("Failed to create Firefox profile dir: {e}")),
        };
    }

    // Start Firefox.
    let mut child = match Command::new(browser_path)
        .args([
            "--headless",
            "--marionette",
            &format!("--marionette-port={port}"),
            "--no-remote",
            "--profile",
            &profile_dir,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&profile_dir);
            return ToolOutput {
                output: None,
                error: Some(format!("Failed to spawn Firefox: {e}")),
            };
        }
    };

    let result = firefox_marionette_session(port, url, timeout);

    // Kill Firefox.
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();

    // Clean up profile.
    let _ = std::fs::remove_dir_all(&profile_dir);

    result
}

/// Connect to Marionette, navigate, extract HTML, and return a ToolOutput.
fn firefox_marionette_session(port: u16, url: &str, timeout: Duration) -> ToolOutput {
    use std::net::TcpStream;

    // Retry connecting until Marionette is ready.
    let deadline = std::time::Instant::now() + timeout;
    let mut stream = loop {
        if std::time::Instant::now() > deadline {
            return ToolOutput {
                output: None,
                error: Some(format!(
                    "Timed out waiting for Firefox Marionette on port {port}"
                )),
            };
        }
        match TcpStream::connect(format!("127.0.0.1:{port}")) {
            Ok(s) => break s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
    };

    // Set a read/write timeout on the stream.
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let _ = stream.set_read_timeout(Some(remaining));
    let _ = stream.set_write_timeout(Some(remaining));

    // Read the handshake message.
    if let Err(e) = marionette_recv(&mut stream) {
        return ToolOutput {
            output: None,
            error: Some(format!("Failed to read Marionette handshake: {e}")),
        };
    }

    // NewSession
    if let Err(e) = marionette_call(
        &mut stream,
        1,
        "WebDriver:NewSession",
        serde_json::json!({}),
    ) {
        return ToolOutput {
            output: None,
            error: Some(format!("Marionette NewSession failed: {e}")),
        };
    }

    // Navigate
    if let Err(e) = marionette_call(
        &mut stream,
        2,
        "WebDriver:Navigate",
        serde_json::json!({"url": url}),
    ) {
        let _ = marionette_call(
            &mut stream,
            99,
            "WebDriver:DeleteSession",
            serde_json::json!({}),
        );
        return ToolOutput {
            output: None,
            error: Some(format!("Marionette Navigate failed: {e}")),
        };
    }

    // Extract rendered HTML.
    let html_result = marionette_call(
        &mut stream,
        3,
        "WebDriver:ExecuteScript",
        serde_json::json!({
            "script": "return document.documentElement.outerHTML"
        }),
    );

    // Always try to clean up the session.
    let _ = marionette_call(
        &mut stream,
        4,
        "WebDriver:DeleteSession",
        serde_json::json!({}),
    );

    match html_result {
        Ok(result) => {
            let html = result
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if html.is_empty() {
                ToolOutput {
                    output: None,
                    error: Some("Firefox returned empty HTML".to_string()),
                }
            } else {
                ToolOutput {
                    output: Some(html),
                    error: None,
                }
            }
        }
        Err(e) => ToolOutput {
            output: None,
            error: Some(format!("Marionette ExecuteScript failed: {e}")),
        },
    }
}

/// Wait for a child process with a timeout. Returns (ExitStatus, stdout, stderr).
fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), String> {
    use std::sync::mpsc;
    use std::thread;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut child = child;
        let stdout = child
            .stdout
            .take()
            .map(|mut r| {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
            .unwrap_or_default();
        let stderr = child
            .stderr
            .take()
            .map(|mut r| {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
            .unwrap_or_default();
        let status = child.wait();
        let _ = tx.send((status, stdout, stderr));
    });

    match rx.recv_timeout(timeout) {
        Ok((Ok(status), stdout, stderr)) => Ok((status, stdout, stderr)),
        Ok((Err(e), _, _)) => Err(format!("Failed to wait on process: {e}")),
        Err(_) => {
            // Timeout — kill the process.
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            Err(format!("Command timed out after {timeout:?}"))
        }
    }
}

fn build_http_agent() -> ureq::Agent {
    let timeout_secs = std::env::var("PHYLACTERY_TOOL_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_secs))
        .redirects(10)
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0",
        )
        .build()
}

fn http_request(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &Map<String, serde_json::Value>,
) -> ToolOutput {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolOutput {
            output: None,
            error: Some("URL must start with http:// or https://".to_string()),
        };
    }

    let agent = build_http_agent();

    let mut request = match method {
        "GET" => agent.get(url),
        "POST" => agent.post(url),
        "PUT" => agent.put(url),
        _ => {
            return ToolOutput {
                output: None,
                error: Some(format!("Unsupported HTTP method: {method}")),
            };
        }
    };

    for (name, value) in headers {
        if let Some(v) = value.as_str() {
            request = request.set(name, v);
        }
    }

    let include_status = method != "GET";

    let result = if method == "GET" {
        request.call()
    } else {
        request.send_string(body.unwrap_or(""))
    };

    match result {
        Ok(response) => {
            if include_status {
                let status = response.status();
                let status_text = response.status_text().to_string();
                let mut resp_headers = String::new();
                for name in response.headers_names() {
                    if let Some(value) = response.header(&name) {
                        resp_headers.push_str(&format!("{name}: {value}\n"));
                    }
                }
                match response.into_string() {
                    Ok(resp_body) => ToolOutput {
                        output: Some(format!(
                            "HTTP {status} {status_text}\n{resp_headers}\n{resp_body}"
                        )),
                        error: None,
                    },
                    Err(e) => ToolOutput {
                        output: None,
                        error: Some(format!("Failed to read response body: {e}")),
                    },
                }
            } else {
                match response.into_string() {
                    Ok(resp_body) => ToolOutput {
                        output: Some(resp_body),
                        error: None,
                    },
                    Err(e) => ToolOutput {
                        output: None,
                        error: Some(format!("Failed to read response body: {e}")),
                    },
                }
            }
        }
        Err(ureq::Error::Status(code, response)) if include_status => {
            let status_text = response.status_text().to_string();
            let mut resp_headers = String::new();
            for name in response.headers_names() {
                if let Some(value) = response.header(&name) {
                    resp_headers.push_str(&format!("{name}: {value}\n"));
                }
            }
            match response.into_string() {
                Ok(resp_body) => ToolOutput {
                    output: Some(format!(
                        "HTTP {code} {status_text}\n{resp_headers}\n{resp_body}"
                    )),
                    error: None,
                },
                Err(e) => ToolOutput {
                    output: None,
                    error: Some(format!(
                        "HTTP {code} {status_text} (failed to read body: {e})"
                    )),
                },
            }
        }
        Err(e) => ToolOutput {
            output: None,
            error: Some(format!("HTTP request failed: {e}")),
        },
    }
}

/// URL-encode a query string for use in a URL parameter.
fn url_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                encoded.push('%');
                encoded.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
                encoded.push(char::from(b"0123456789ABCDEF"[(byte & 0x0F) as usize]));
            }
        }
    }
    encoded
}

/// Percent-decode a string (e.g. from a URL query parameter).
fn percent_decode(input: &str) -> String {
    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            decoded.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            decoded.push(b' ');
        } else {
            decoded.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Strip HTML tags from a string, returning plain text.
fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

/// Extract the value of a query parameter from a URL string.
fn extract_query_param<'a>(url: &'a str, param: &str) -> Option<&'a str> {
    let query_start = url.find('?').map(|i| i + 1).unwrap_or(url.len());
    let query = &url[query_start..];
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && key == param
        {
            return Some(value);
        }
    }
    None
}

/// A single search result parsed from DuckDuckGo HTML.
#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo HTML search results page into structured results.
fn parse_ddg_results(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Split on result__a anchors to find each result title+link
    for chunk in html.split("class=\"result__a\"") {
        if results.len() >= 20 {
            break;
        }
        // Skip the first chunk (before any result)
        // Extract href from this anchor tag
        // The chunk starts inside the <a> tag after class="result__a"
        // Pattern: ... href="..." ...>TITLE</a>
        let href = if let Some(href_start) = chunk.find("href=\"") {
            let start = href_start + 6;
            if let Some(end) = chunk[start..].find('"') {
                &chunk[start..start + end]
            } else {
                continue;
            }
        } else {
            continue;
        };

        // Extract real URL from DDG redirect
        let url = if let Some(uddg) = extract_query_param(href, "uddg") {
            percent_decode(uddg)
        } else {
            // If no redirect, use href directly (strip leading //)
            let h = href.trim_start_matches("//");
            if h.starts_with("http") {
                h.to_string()
            } else {
                format!("https://{h}")
            }
        };

        // Extract title: text between > and </a>
        let title = if let Some(close_bracket) = chunk.find('>') {
            let after = &chunk[close_bracket + 1..];
            if let Some(end_tag) = after.find("</a>") {
                strip_html_tags(&after[..end_tag]).trim().to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if title.is_empty() {
            continue;
        }

        // Find snippet in the same result block
        let snippet = if let Some(snip_pos) = chunk.find("class=\"result__snippet\"") {
            let after_class = &chunk[snip_pos..];
            if let Some(gt) = after_class.find('>') {
                let content = &after_class[gt + 1..];
                if let Some(end) = content.find("</a>") {
                    strip_html_tags(&content[..end]).trim().to_string()
                } else if let Some(end) = content.find("</span>") {
                    strip_html_tags(&content[..end]).trim().to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

fn web_search(query: &str) -> ToolOutput {
    if query.trim().is_empty() {
        return ToolOutput {
            output: None,
            error: Some("Search query cannot be empty".to_string()),
        };
    }

    let encoded_query = url_encode(query);
    let search_url = format!("https://html.duckduckgo.com/html/?q={encoded_query}");

    let agent = build_http_agent();

    let html = match agent.get(&search_url).call() {
        Ok(response) => match response.into_string() {
            Ok(body) => body,
            Err(e) => {
                return ToolOutput {
                    output: None,
                    error: Some(format!("Failed to read response body: {e}")),
                };
            }
        },
        Err(e) => {
            return ToolOutput {
                output: None,
                error: Some(format!("Search request failed: {e}")),
            };
        }
    };

    let results = parse_ddg_results(&html);

    if results.is_empty() {
        return ToolOutput {
            output: Some(format!("No results found for: {query}")),
            error: None,
        };
    }

    let mut output = String::new();
    for (i, result) in results.iter().enumerate() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("{}. {}\n", i + 1, result.title));
        output.push_str(&format!("   {}\n", result.url));
        if !result.snippet.is_empty() {
            output.push_str(&format!("   {}\n", result.snippet));
        }
    }

    ToolOutput {
        output: Some(output),
        error: None,
    }
}

fn html_to_markdown(html: &str) -> String {
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "nav", "footer", "header", "noscript", "svg", "img", "picture",
            "video", "audio", "iframe", "object", "embed", "form", "input", "button", "select",
            "textarea",
        ])
        .build();

    let md = converter.convert(html).unwrap_or_default();

    // Collapse 3+ consecutive newlines down to 2.
    let mut result = String::with_capacity(md.len());
    let mut newline_count = 0;
    for ch in md.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push(ch);
            }
        } else {
            newline_count = 0;
            result.push(ch);
        }
    }
    result
}

fn web_read(url: &str, headers: &Map<String, serde_json::Value>) -> ToolOutput {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolOutput {
            output: None,
            error: Some("URL must start with http:// or https://".to_string()),
        };
    }

    let agent = build_http_agent();
    let mut request = agent.get(url);
    for (name, value) in headers {
        if let Some(v) = value.as_str() {
            request = request.set(name, v);
        }
    }

    let html = match request.call() {
        Ok(response) => match response.into_string() {
            Ok(body) => body,
            Err(e) => {
                return ToolOutput {
                    output: None,
                    error: Some(format!("Failed to read response body: {e}")),
                };
            }
        },
        Err(e) => {
            return ToolOutput {
                output: None,
                error: Some(format!("HTTP request failed: {e}")),
            };
        }
    };

    let md = html_to_markdown(&html);
    if md.trim().is_empty() {
        ToolOutput {
            output: Some("(page returned no readable content)".to_string()),
            error: None,
        }
    } else {
        ToolOutput {
            output: Some(md),
            error: None,
        }
    }
}

fn web_fetch(url: &str) -> ToolOutput {
    // Validate URL scheme.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ToolOutput {
            output: None,
            error: Some("URL must start with http:// or https://".to_string()),
        };
    }

    let browser = match detect_browser() {
        Some(b) => b,
        None => {
            return ToolOutput {
                output: None,
                error: Some(
                    "No supported browser found. Install Chrome, Chromium, or Firefox.".to_string(),
                ),
            };
        }
    };

    let timeout_secs = std::env::var("PHYLACTERY_TOOL_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    match browser {
        Browser::Chrome(path) => fetch_with_chromium(&path, url, timeout),
        Browser::Firefox(path) => fetch_with_firefox(&path, url, timeout),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--spec") {
        let specs = tool_specs();
        println!(
            "{}",
            serde_json::to_string_pretty(&specs).expect("failed to serialize specs")
        );
        return;
    }

    // Read ToolInput from stdin.
    let mut input_str = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input_str) {
        let err = ToolOutput {
            output: None,
            error: Some(format!("Failed to read stdin: {e}")),
        };
        println!("{}", serde_json::to_string(&err).unwrap());
        std::process::exit(1);
    }

    let input: ToolInput = match serde_json::from_str(&input_str) {
        Ok(v) => v,
        Err(e) => {
            let err = ToolOutput {
                output: None,
                error: Some(format!("Invalid JSON input: {e}")),
            };
            println!("{}", serde_json::to_string(&err).unwrap());
            std::process::exit(1);
        }
    };

    let empty_headers = Map::new();

    let result = match input.name.as_str() {
        "http_fetch" | "http_post" | "http_put" => {
            let url = match input.arguments.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    let err = ToolOutput {
                        output: None,
                        error: Some("Missing required argument: url".to_string()),
                    };
                    println!("{}", serde_json::to_string(&err).unwrap());
                    std::process::exit(1);
                }
            };
            let headers = input
                .arguments
                .get("headers")
                .and_then(|v| v.as_object())
                .unwrap_or(&empty_headers);
            let body = input.arguments.get("body").and_then(|v| v.as_str());
            let method = match input.name.as_str() {
                "http_post" => "POST",
                "http_put" => "PUT",
                _ => "GET",
            };
            http_request(method, url, body, headers)
        }
        "web_read" => {
            let url = match input.arguments.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    let err = ToolOutput {
                        output: None,
                        error: Some("Missing required argument: url".to_string()),
                    };
                    println!("{}", serde_json::to_string(&err).unwrap());
                    std::process::exit(1);
                }
            };
            let headers = input
                .arguments
                .get("headers")
                .and_then(|v| v.as_object())
                .unwrap_or(&empty_headers);
            web_read(url, headers)
        }
        "web_fetch" => {
            let url = match input.arguments.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    let err = ToolOutput {
                        output: None,
                        error: Some("Missing required argument: url".to_string()),
                    };
                    println!("{}", serde_json::to_string(&err).unwrap());
                    std::process::exit(1);
                }
            };
            web_fetch(url)
        }
        "web_search" => {
            let query = match input.arguments.get("query").and_then(|v| v.as_str()) {
                Some(q) => q,
                None => {
                    let err = ToolOutput {
                        output: None,
                        error: Some("Missing required argument: query".to_string()),
                    };
                    println!("{}", serde_json::to_string(&err).unwrap());
                    std::process::exit(1);
                }
            };
            web_search(query)
        }
        other => ToolOutput {
            output: None,
            error: Some(format!("Unknown tool: {other}")),
        },
    };
    println!("{}", serde_json::to_string(&result).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_specs_returns_all() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 6);
        assert_eq!(specs[0].name, "http_fetch");
        assert_eq!(specs[1].name, "http_post");
        assert_eq!(specs[2].name, "http_put");
        assert_eq!(specs[3].name, "web_read");
        assert_eq!(specs[4].name, "web_fetch");
        assert_eq!(specs[5].name, "web_search");
        for spec in &specs {
            assert_eq!(spec.mode, ToolMode::Oneshot);
            assert!(spec.sandbox.as_ref().unwrap().net);
        }
    }

    #[test]
    fn test_web_search_spec_has_query_param() {
        let specs = tool_specs();
        let search_spec = specs.iter().find(|s| s.name == "web_search").unwrap();
        let required = search_spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "query");
        assert!(search_spec.parameters["properties"]["query"].is_object());
    }

    #[test]
    fn test_http_post_spec() {
        let specs = tool_specs();
        let spec = specs.iter().find(|s| s.name == "http_post").unwrap();
        let required = spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "url");
        assert!(spec.parameters["properties"]["body"].is_object());
        assert!(spec.parameters["properties"]["headers"].is_object());
    }

    #[test]
    fn test_http_put_spec() {
        let specs = tool_specs();
        let spec = specs.iter().find(|s| s.name == "http_put").unwrap();
        let required = spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "url");
        assert!(spec.parameters["properties"]["body"].is_object());
        assert!(spec.parameters["properties"]["headers"].is_object());
    }

    #[test]
    fn test_http_fetch_spec_has_headers() {
        let specs = tool_specs();
        let spec = specs.iter().find(|s| s.name == "http_fetch").unwrap();
        assert!(spec.parameters["properties"]["headers"].is_object());
    }

    #[test]
    fn test_tool_specs_valid_json() {
        let specs = tool_specs();
        let json = serde_json::to_string(&specs).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 6);
    }

    #[test]
    fn test_http_fetch_rejects_non_http() {
        let result = http_request("GET", "ftp://example.com", None, &Map::new());
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn test_http_post_rejects_non_http() {
        let result = http_request("POST", "ftp://example.com", None, &Map::new());
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn test_http_put_rejects_non_http() {
        let result = http_request("PUT", "ftp://example.com", None, &Map::new());
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn test_http_request_empty_headers() {
        // Just verifies empty headers don't cause issues (will fail on DNS but that's fine)
        let result = http_request("GET", "http://localhost:1", None, &Map::new());
        // Should get a connection error, not a header-related error
        assert!(result.error.is_some());
    }

    #[test]
    fn test_web_fetch_rejects_non_http() {
        let result = web_fetch("ftp://example.com");
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn test_find_executable_nonexistent() {
        assert!(find_executable("this-binary-does-not-exist-xyz123").is_none());
    }

    #[test]
    fn test_find_executable_absolute_nonexistent() {
        assert!(find_executable("/nonexistent/path/to/binary").is_none());
    }

    #[test]
    fn test_detect_browser_returns_option() {
        // Just verify it doesn't panic — result depends on the host system.
        let _ = detect_browser();
    }

    #[test]
    fn test_url_encode_basic() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("rust lang"), "rust+lang");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("simple"), "simple");
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a%26b"), "a&b");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("no%encoding"), "no%encoding"); // invalid sequence preserved
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags here"), "no tags here");
        assert_eq!(strip_html_tags("<a href=\"x\">link</a>"), "link");
    }

    #[test]
    fn test_extract_query_param() {
        assert_eq!(
            extract_query_param("http://example.com?foo=bar&baz=qux", "foo"),
            Some("bar")
        );
        assert_eq!(
            extract_query_param("http://example.com?foo=bar&baz=qux", "baz"),
            Some("qux")
        );
        assert_eq!(
            extract_query_param("http://example.com?foo=bar", "missing"),
            None
        );
    }

    #[test]
    fn test_parse_ddg_results() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&rut=abc">
                <b>Rust</b> Programming Language
            </a>
            <a class="result__snippet">A language empowering everyone to build <b>reliable</b> software.</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FRust&rut=def">
                Rust - Wikipedia
            </a>
            <a class="result__snippet">Rust is a systems programming language.</a>
        </div>
        "#;

        let results = parse_ddg_results(html);
        assert_eq!(results.len(), 2);

        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "A language empowering everyone to build reliable software."
        );

        assert_eq!(results[1].title, "Rust - Wikipedia");
        assert_eq!(results[1].url, "https://en.wikipedia.org/wiki/Rust");
        assert_eq!(
            results[1].snippet,
            "Rust is a systems programming language."
        );
    }

    #[test]
    fn test_parse_ddg_results_caps_at_20() {
        // Generate HTML with 25 fake results
        let mut html = String::new();
        for i in 0..25 {
            html.push_str(&format!(
                r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F{i}&rut=x">Result {i}</a>
                <a class="result__snippet">Snippet {i}</a>"#
            ));
        }
        let results = parse_ddg_results(&html);
        assert_eq!(results.len(), 20);
    }

    #[test]
    fn test_web_search_empty_query() {
        let result = web_search("");
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("empty"));
    }

    #[test]
    fn test_web_read_spec() {
        let specs = tool_specs();
        let spec = specs.iter().find(|s| s.name == "web_read").unwrap();
        let required = spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "url");
        assert!(spec.parameters["properties"]["headers"].is_object());
    }

    #[test]
    fn test_web_read_rejects_non_http() {
        let result = web_read("ftp://example.com", &Map::new());
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .unwrap()
                .contains("must start with http:// or https://")
        );
    }

    #[test]
    fn test_html_to_markdown_strips_tags() {
        let html = r#"<html><head><script>alert('x')</script><style>body{}</style></head>
            <body><nav><a href="/">Home</a></nav>
            <h1>Hello World</h1><p>This is a <strong>test</strong> paragraph.</p>
            <footer>Copyright 2024</footer></body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("Hello World"));
        assert!(md.contains("test"));
        assert!(md.contains("paragraph"));
        assert!(!md.contains("alert"));
        assert!(!md.contains("body{}"));
        assert!(!md.contains("Copyright"));
        assert!(!md.contains("Home"));
    }

    #[test]
    fn test_html_to_markdown_collapses_blank_lines() {
        let html = "<p>First</p>\n\n\n\n\n<p>Second</p>";
        let md = html_to_markdown(html);
        assert!(!md.contains("\n\n\n"));
    }
}
