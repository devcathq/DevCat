//! Tests: a GitHub REST API on 127.0.0.1 for one repository, made from data. The test fills a
//! `Gh` (the token owner, the repository's labels, issues, comments, label events, collaborator
//! permissions, blockers, parents, pull requests, branches), starts it with `MockGithub::start`, and points
//! a board's site at `MockGithub::url`. Writes change the data the way GitHub would (labels on
//! issues, new comments, pull requests, closed issues), so later reads and the test see them.
//! Every request is recorded as `METHOD /path?query body`.
//!
//! A case the routes don't cover yet: add it to `Gh::canned` (answers that win over the routes,
//! e.g. a 401 for `GET /user`), or add a route in `answer`.
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// What the mock GitHub knows.
#[derive(Debug, Clone, Default)]
pub struct Gh {
    /// The token's owner (`GET /user`).
    pub login: String,
    /// `owner/name`: the one repository it serves.
    pub repo: String,
    pub default_branch: String,
    /// The token may push to the repository (`permissions.push` of `GET /repos/{o}/{r}`).
    pub push: bool,
    /// The repository's labels.
    pub labels: Vec<String>,
    /// Issues by number, as GitHub sends them (see `Gh::issue`). One with a `pull_request` key is a
    /// pull request listed as an issue.
    pub issues: BTreeMap<u64, Value>,
    /// Comments on each issue, oldest first (`{id, user: {login}, body, created_at}`).
    pub comments: HashMap<u64, Vec<Value>>,
    /// Each issue's events, oldest first (`{event, created_at, actor: {login}, label: {name}}`).
    pub events: HashMap<u64, Vec<Value>>,
    /// Collaborator permission by login: `admin`, `write` or `read` (anyone else: `none`).
    pub permissions: HashMap<String, String>,
    /// Issues blocking each issue (`issues/{n}/dependencies/blocked_by`), by number.
    pub blocked_by: HashMap<u64, Vec<u64>>,
    /// Each sub-issue's parent issue (`issues/{n}/parent`).
    pub parents: HashMap<u64, u64>,
    /// Pull requests by number (`{number, html_url, state, draft, merged, title, body, head: {ref,
    /// sha}, base: {ref}}`); the ones DevCat opens land here too.
    pub pulls: BTreeMap<u64, Value>,
    /// The repository's branches on GitHub and the commit each points at (`git/ref/heads/{b}`).
    /// The tests' git remote is a bare repository, so a branch DevCat pushes isn't here by itself.
    pub refs: HashMap<String, String>,
    /// Answers that win over the routes: (`"METHOD /path"` without the query, status, body).
    pub canned: Vec<(String, u16, Value)>,
    /// Every request, `METHOD /path?query body`.
    pub requests: Vec<String>,
}

impl Gh {
    /// `repo` (`owner/name`) with `main` as its default branch, a token of `login` that may push,
    /// and the labels GitHub starts a repository with that matter here.
    pub fn new(login: &str, repo: &str) -> Gh {
        Gh { login: login.into(), repo: repo.into(), default_branch: "main".into(), push: true, labels: vec!["bug".into(), "ready-for-agent".into()], ..Default::default() }
    }

    /// An open issue as GitHub sends it, with `labels` and `assignees` (logins).
    pub fn issue(&self, n: u64, title: &str, body: &str, labels: &[&str], assignees: &[&str]) -> Value {
        json!({
            "number": n, "title": title, "body": body, "state": "open", "state_reason": null,
            "html_url": format!("https://github.com/{}/issues/{n}", self.repo),
            "labels": labels.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>(),
            "assignees": assignees.iter().map(|a| json!({ "login": a })).collect::<Vec<_>>(),
            "user": { "login": self.login }, "created_at": "2026-11-19T07:00:00Z",
            "issue_dependencies_summary": { "blocked_by": 0, "total_blocked_by": 0, "blocking": 0, "total_blocking": 0 },
            "parent_issue_url": null,
        })
    }

    /// Adds an open issue (see `issue`).
    pub fn with_issue(mut self, n: u64, title: &str, body: &str, labels: &[&str], assignees: &[&str]) -> Gh {
        let i = self.issue(n, title, body, labels, assignees);
        self.issues.insert(n, i);
        self
    }

    /// An issue's label names.
    pub fn labels_of(&self, n: u64) -> Vec<String> {
        self.issues.get(&n).map(|i| names(&i["labels"])).unwrap_or_default()
    }

    /// The next issue or pull request number (they share one sequence, as on GitHub).
    fn next_number(&self) -> u64 {
        self.issues.keys().chain(self.pulls.keys()).max().copied().unwrap_or(0) + 1
    }
}

fn names(labels: &Value) -> Vec<String> {
    labels.as_array().into_iter().flatten().filter_map(|l| l["name"].as_str()).map(str::to_string).collect()
}

/// A running mock GitHub.
pub struct MockGithub {
    /// The API address: a board's site.
    pub url: String,
    pub gh: Arc<Mutex<Gh>>,
}

impl MockGithub {
    pub async fn start(gh: Gh) -> MockGithub {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let state = Arc::new(Mutex::new(gh));
        let data = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { break };
                let data = data.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (head, body) = loop {
                        let n = s.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i]).to_string();
                            let len = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                            if buf.len() >= i + 4 + len {
                                break (head, String::from_utf8_lossy(&buf[i + 4..i + 4 + len]).to_string());
                            }
                        }
                    };
                    let line = head.lines().next().unwrap_or("").to_string();
                    let method = line.split(' ').next().unwrap_or("").to_string();
                    let target = line.split(' ').nth(1).unwrap_or("").to_string();
                    let (status, out) = {
                        let mut gh = data.lock();
                        gh.requests.push(format!("{method} {target} {body}").trim_end().to_string());
                        answer(&mut gh, &method, &target, &body)
                    };
                    let out = if status == 204 { String::new() } else { out.to_string() };
                    let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(out.as_bytes()).await;
                });
            }
        });
        MockGithub { url, gh: state }
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<String> {
        self.gh.lock().requests.clone()
    }

    /// The requests that start with `prefix` (e.g. `POST /repos/acme/calc/pulls`).
    pub fn sent(&self, prefix: &str) -> Vec<String> {
        self.gh.lock().requests.iter().filter(|r| r.starts_with(prefix)).cloned().collect()
    }

    pub fn labels_of(&self, n: u64) -> Vec<String> {
        self.gh.lock().labels_of(n)
    }

    /// The comments' texts on an issue, oldest first.
    pub fn comments_on(&self, n: u64) -> Vec<String> {
        self.gh.lock().comments.get(&n).into_iter().flatten().map(|c| c["body"].as_str().unwrap_or_default().to_string()).collect()
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn not_found() -> (u16, Value) {
    (404, json!({ "message": "Not Found" }))
}

/// GitHub's answer to one request, changing the data as GitHub would.
fn answer(gh: &mut Gh, method: &str, target: &str, body: &str) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q: HashMap<String, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect();
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if let Some((_, status, v)) = gh.canned.iter().find(|(r, _, _)| *r == format!("{method} {path}")) {
        return (*status, v.clone());
    }
    let page: usize = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    if method == "GET" && path == "/user" {
        return (200, json!({ "login": gh.login, "id": 1, "type": "User" }));
    }
    let Some(rest) = path.strip_prefix(&format!("/repos/{}", gh.repo)) else { return not_found() };
    let parts: Vec<String> = rest.split('/').skip(1).map(decode).collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let num = |s: &str| s.parse::<u64>().ok();
    match (method, parts.as_slice()) {
        ("GET", []) => (200, json!({ "full_name": gh.repo, "default_branch": gh.default_branch, "private": true, "has_issues": true,
            "permissions": { "admin": false, "maintain": false, "push": gh.push, "triage": gh.push, "pull": true } })),
        ("GET", ["labels"]) => (200, if page > 1 { json!([]) } else { json!(gh.labels.iter().map(|l| json!({ "name": l, "color": "ededed" })).collect::<Vec<_>>()) }),
        ("POST", ["labels"]) => {
            let name = body["name"].as_str().unwrap_or_default().to_string();
            if gh.labels.iter().any(|l| l.eq_ignore_ascii_case(&name)) {
                return (422, json!({ "message": "Validation Failed", "errors": [{ "code": "already_exists" }] }));
            }
            gh.labels.push(name.clone());
            (201, json!({ "name": name, "color": body["color"] }))
        }
        ("GET", ["labels", name]) => match gh.labels.iter().find(|l| l == name) {
            Some(l) => (200, json!({ "name": l })),
            None => not_found(),
        },
        ("GET", ["issues"]) => {
            if page > 1 {
                return (200, json!([]));
            }
            let state = q.get("state").map(String::as_str).unwrap_or("open");
            let want: Vec<&str> = q.get("labels").map(|l| l.split(',').filter(|x| !x.is_empty()).collect()).unwrap_or_default();
            let list: Vec<Value> = gh
                .issues
                .values()
                .filter(|i| state == "all" || i["state"] == state)
                .filter(|i| want.iter().all(|w| names(&i["labels"]).iter().any(|l| l == w)))
                .filter(|i| {
                    let who: Vec<&str> = i["assignees"].as_array().into_iter().flatten().filter_map(|a| a["login"].as_str()).collect();
                    match q.get("assignee").map(String::as_str) {
                        None => true,
                        Some("*") => !who.is_empty(),
                        Some("none") => who.is_empty(),
                        Some(login) => who.contains(&login),
                    }
                })
                .cloned()
                .collect();
            (200, json!(list))
        }
        (_, ["issues", n, ..]) if num(n).is_none_or(|n| !gh.issues.contains_key(&n)) => not_found(),
        ("GET", ["issues", n]) => (200, gh.issues[&num(n).unwrap()].clone()),
        ("PATCH", ["issues", n]) => {
            let i = gh.issues.get_mut(&num(n).unwrap()).unwrap();
            for (k, v) in body.as_object().into_iter().flatten() {
                i[k] = v.clone();
            }
            (200, i.clone())
        }
        ("GET", ["issues", n, "comments"]) => (200, json!(gh.comments.get(&num(n).unwrap()).cloned().unwrap_or_default())),
        ("POST", ["issues", n, "comments"]) => {
            let n = num(n).unwrap();
            let id = gh.comments.values().map(Vec::len).sum::<usize>() + 1;
            let c = json!({ "id": id, "user": { "login": gh.login, "type": "User" }, "body": body["body"], "created_at": "2026-11-19T08:00:00Z",
                "html_url": format!("https://github.com/{}/issues/{n}#issuecomment-{id}", gh.repo) });
            gh.comments.entry(n).or_default().push(c.clone());
            (201, c)
        }
        ("GET", ["issues", n, "events"]) => (200, if page > 1 { json!([]) } else { json!(gh.events.get(&num(n).unwrap()).cloned().unwrap_or_default()) }),
        ("POST" | "PUT", ["issues", n, "labels"]) => {
            let n = num(n).unwrap();
            let add = body["labels"].as_array().cloned().unwrap_or_default();
            let mut now = if method == "PUT" { vec![] } else { gh.labels_of(n) };
            for l in add.iter().filter_map(Value::as_str) {
                // GitHub makes a label it doesn't know
                if !gh.labels.iter().any(|x| x == l) {
                    gh.labels.push(l.to_string());
                }
                if !now.iter().any(|x| x == l) {
                    now.push(l.to_string());
                }
            }
            let labels = json!(now.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>());
            gh.issues.get_mut(&n).unwrap()["labels"] = labels.clone();
            (200, labels)
        }
        ("DELETE", ["issues", n, "labels", name]) => {
            let n = num(n).unwrap();
            let now = gh.labels_of(n);
            if !now.iter().any(|l| l == name) {
                return (404, json!({ "message": "Label does not exist" }));
            }
            let labels = json!(now.iter().filter(|l| l != name).map(|l| json!({ "name": l })).collect::<Vec<_>>());
            gh.issues.get_mut(&n).unwrap()["labels"] = labels.clone();
            (200, labels)
        }
        ("GET", ["issues", n, "dependencies", "blocked_by"]) => {
            let list: Vec<Value> = gh.blocked_by.get(&num(n).unwrap()).into_iter().flatten().filter_map(|b| gh.issues.get(b).cloned()).collect();
            (200, json!(list))
        }
        ("GET", ["issues", n, "parent"]) => match gh.parents.get(&num(n).unwrap()).and_then(|p| gh.issues.get(p)) {
            Some(p) => (200, p.clone()),
            None => not_found(),
        },
        ("GET", ["collaborators", login, "permission"]) => {
            let p = gh.permissions.get(*login).cloned().unwrap_or_else(|| "none".into());
            let push = p == "admin" || p == "write";
            (200, json!({ "permission": p, "role_name": p, "user": { "login": login, "permissions": { "admin": p == "admin", "push": push, "pull": p != "none" } } }))
        }
        ("POST", ["pulls"]) => {
            let head = body["head"].as_str().unwrap_or_default().to_string();
            let head = head.split_once(':').map(|(_, b)| b.to_string()).unwrap_or(head);
            if gh.pulls.values().any(|p| p["state"] == "open" && p["head"]["ref"] == head.as_str()) {
                return (422, json!({ "message": "Validation Failed", "errors": [{ "message": format!("A pull request already exists for {}:{head}.", gh.repo.split('/').next().unwrap_or_default()) }] }));
            }
            let n = gh.next_number();
            let p = json!({ "number": n, "html_url": format!("https://github.com/{}/pull/{n}", gh.repo), "state": "open", "draft": body["draft"], "merged": false,
                "merged_at": null, "title": body["title"], "body": body["body"], "head": { "ref": head, "sha": "" }, "base": { "ref": body["base"] } });
            gh.pulls.insert(n, p.clone());
            (201, p)
        }
        ("GET", ["pulls"]) => {
            let state = q.get("state").map(String::as_str).unwrap_or("open");
            let head = q.get("head").map(|h| h.split_once(':').map(|(_, b)| b.to_string()).unwrap_or(h.clone()));
            let list: Vec<Value> = gh.pulls.values().filter(|p| state == "all" || p["state"] == state).filter(|p| head.as_ref().is_none_or(|h| p["head"]["ref"] == h.as_str())).cloned().collect();
            (200, json!(list))
        }
        (_, ["pulls", n]) if num(n).is_none_or(|n| !gh.pulls.contains_key(&n)) => not_found(),
        ("GET", ["pulls", n]) => (200, gh.pulls[&num(n).unwrap()].clone()),
        ("PATCH", ["pulls", n]) => {
            let p = gh.pulls.get_mut(&num(n).unwrap()).unwrap();
            for (k, v) in body.as_object().into_iter().flatten() {
                p[k] = v.clone();
            }
            (200, p.clone())
        }
        ("GET", ["git", "ref", "heads", branch @ ..]) => match gh.refs.get(&branch.join("/")) {
            Some(sha) => (200, json!({ "ref": format!("refs/heads/{}", branch.join("/")), "object": { "type": "commit", "sha": sha } })),
            None => not_found(),
        },
        ("DELETE", ["git", "refs", "heads", branch @ ..]) => match gh.refs.remove(&branch.join("/")) {
            Some(_) => (204, Value::Null),
            None => (422, json!({ "message": "Reference does not exist" })),
        },
        _ => not_found(),
    }
}
