//! `web_fetch`: read a web page (or any http(s) URL) as plain text.
use super::{Tool, ToolCtx, ToolOutput, str_arg};
use crate::approval::Kind;
use crate::message::ToolSpec;
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;
use std::time::Duration;

/// Bytes read from a response at most.
const MAX_BODY: usize = 5 * 1024 * 1024;

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch an http(s) URL and return it as text: web pages are converted to plain text (links kept as `text (url)`), JSON and text come back as they are. Use it for documentation, issues, changelogs and API references the user points to or that you need. Don't put secrets or project contents in the URL.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "http:// or https:// URL" } },
                "required": ["url"]
            }),
        }
    }
    fn kind(&self, _: &Value, _: &ToolCtx) -> Kind {
        Kind::Fetch
    }
    fn summary(&self, input: &Value) -> String {
        input.get("url").and_then(Value::as_str).unwrap_or("?").to_string()
    }
    fn timeout(&self, _: &Value) -> Duration {
        Duration::from_secs(45)
    }
    async fn run(&self, input: Value, _: &ToolCtx) -> ToolOutput {
        let url = match str_arg(&input, "url") {
            Ok(u) => u.trim(),
            Err(e) => return e,
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return ToolOutput::err("Only http:// and https:// URLs can be fetched.");
        }
        let http = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .user_agent(concat!("DevCat-Agent/", env!("CARGO_PKG_VERSION")))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let resp = match http.get(url).header("accept", "text/html,application/xhtml+xml,text/plain,application/json;q=0.9,*/*;q=0.5").send().await {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("Couldn't fetch {url}: {e}")),
        };
        let status = resp.status();
        let final_url = resp.url().to_string();
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        let body = match read_capped(resp).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("Couldn't read {url}: {e}")),
        };
        let textual = ctype.is_empty() || ctype.starts_with("text/") || ctype.contains("json") || ctype.contains("xml") || ctype.contains("javascript");
        if !textual {
            return ToolOutput::err(format!("{final_url} is {ctype} ({} bytes), not text. To use it, download it with `shell` and read it with a script.", body.len()));
        }
        let raw = String::from_utf8_lossy(&body);
        let text = if ctype.contains("html") || (ctype.is_empty() && raw.trim_start().starts_with('<')) { html_to_text(&raw) } else { raw.into_owned() };
        let head = if final_url != url { format!("{final_url} (redirected from {url})") } else { final_url };
        let out = format!("{head}\nHTTP {status}\n\n{}", text.trim());
        if status.is_success() { ToolOutput::ok(out) } else { ToolOutput::err(out) }
    }
}

async fn read_capped(mut resp: reqwest::Response) -> reqwest::Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() >= MAX_BODY {
            out.truncate(MAX_BODY);
            break;
        }
    }
    Ok(out)
}

static DROP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<(script|style|noscript|svg|template|iframe|head)\b.*?</(script|style|noscript|svg|template|iframe|head)\s*>|<!--.*?-->").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"'#][^"']*)["'][^>]*>(.*?)</a>"#).unwrap());
static BLOCK_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</(p|div|section|article|header|footer|main|nav|aside|tr|table|ul|ol|pre|blockquote|h[1-6])>|<br\s*/?>|<hr\s*/?>").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<h([1-6])\b[^>]*>").unwrap());
static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<li\b[^>]*>").unwrap());
static CELL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</t[dh]>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{a0}]+").unwrap());
static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n(\s*\n)+").unwrap());

/// A web page as readable text: no scripts or styles, headings as `#`, list items as `-`,
/// links as `text (url)`.
pub(crate) fn html_to_text(html: &str) -> String {
    let title = TITLE.captures(html).map(|c| decode(TAG.replace_all(&c[1], "").trim()));
    let s = DROP.replace_all(html, "");
    let s = LINK.replace_all(&s, |c: &regex::Captures| {
        let text = TAG.replace_all(&c[2], "");
        let text = text.trim();
        if text.is_empty() { String::new() } else { format!("{text} ({})", &c[1]) }
    });
    let s = HEADING.replace_all(&s, |c: &regex::Captures| format!("\n\n{} ", "#".repeat(c[1].parse().unwrap_or(1))));
    let s = ITEM.replace_all(&s, "\n- ");
    let s = CELL.replace_all(&s, " | ");
    let s = BLOCK_END.replace_all(&s, "\n");
    let s = TAG.replace_all(&s, "");
    let s = decode(&s);
    let s = SPACES.replace_all(&s, " ");
    let s: String = s.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    let s = BLANKS.replace_all(&s, "\n\n");
    match title {
        Some(t) if !t.is_empty() => format!("Title: {t}\n\n{}", s.trim()),
        _ => s.trim().to_string(),
    }
}

/// The HTML entities that matter for reading.
fn decode(s: &str) -> String {
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);").unwrap());
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(h) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(h, 16).ok().and_then(char::from_u32)
            } else if let Some(d) = e.strip_prefix('#') {
                d.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "mdash" => Some('—'),
                    "ndash" => Some('–'),
                    "hellip" => Some('…'),
                    "copy" => Some('©'),
                    _ => None,
                }
            };
            ch.map(String::from).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<html><head><title>Docs &amp; more</title><style>p{}</style></head><body>
            <script>alert(1)</script><h1>Install</h1><p>Run <code>npm&nbsp;i</code>.</p>
            <ul><li>One</li><li>Two <a href="https://x.dev/a">link</a></li></ul><!-- hidden -->
            <table><tr><td>a</td><td>b</td></tr></table></body></html>"#;
        let t = html_to_text(html);
        assert!(t.starts_with("Title: Docs & more"), "{t}");
        assert!(t.contains("# Install") && t.contains("Run npm i."), "{t}");
        assert!(t.contains("- One") && t.contains("- Two link (https://x.dev/a)"), "{t}");
        assert!(t.contains("a | b"), "{t}");
        assert!(!t.contains("alert") && !t.contains("hidden") && !t.contains("p{}"), "{t}");
    }
}
