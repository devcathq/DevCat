//! GitHub Issues as a tracker: one repository (`owner/name` in the board's project) through the
//! REST API at the board's site (https://api.github.com, or GitHub Enterprise Server's
//! `https://<host>/api/v3`), with a personal access token (fine-grained or classic).
//!
//! Labels are the columns: an issue is "in" the board's state label it has (`ready-for-agent`,
//! `agent-working`, `agent-review`, `ready-for-human`), and moving it swaps that label for the
//! new one, so it keeps exactly one. Saving the board reads the token owner's login (the
//! branches are `<login>/<number>-<slug>`) and creates the state labels the repository lacks. A
//! fixed issue gets a draft pull request into the default branch that closes it.
//!
//! The `Tracker` methods here are what `trackers.rs` calls for a GitHub board; the rest of the
//! pipeline (`worker.rs`) only adds the pull request after the push.
use super::trackers::{Tracker, json_of, s, trello_wanted};
use super::{Assigned, Board, Column, Isolation, RunRecord, Ticket};
use crate::error::{AppError, AppResult};
use reqwest::{Method, RequestBuilder};
use serde_json::{Value, json};

/// Issues read from one label, at most (as for the other trackers' columns).
const MAX_ISSUES: usize = 50;
/// Label pages read for the columns, at most (100 labels each).
const LABEL_PAGES: usize = 10;
/// Event pages read for an issue's trigger label, at most (100 events each).
const EVENT_PAGES: usize = 10;

/// The API address without a trailing slash: the board's site, else https://api.github.com.
pub fn api(b: &Board) -> String {
    let site = b.site.trim().trim_end_matches('/');
    if site.is_empty() {
        "https://api.github.com".into()
    } else if site.starts_with("http://") || site.starts_with("https://") {
        site.into()
    } else {
        format!("https://{site}")
    }
}

/// `owner/name`, nothing else.
pub fn is_repo(project: &str) -> bool {
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    matches!(project.trim().split('/').collect::<Vec<_>>().as_slice(), [o, r] if ok(o) && ok(r))
}

/// A label name as one segment of a URL path.
fn enc(name: &str) -> String {
    name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The color DevCat gives a state label it creates.
fn color(label: &str) -> &'static str {
    match label {
        "ready-for-agent" => "0E8A16",
        "agent-working" => "FBCA04",
        "agent-review" => "1D76DB",
        "ready-for-human" => "D93F0B",
        _ => "C5DEF5",
    }
}

/// The board's state labels (its columns), each once.
pub fn states(b: &Board) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for c in [&b.pick_from, &b.working, &b.done_to, &b.failed_to].into_iter().flatten() {
        if !c.id.is_empty() && !out.contains(&c.id) {
            out.push(c.id.clone());
        }
    }
    out
}

/// A GitHub issue as a ticket: `#<number>`, in the column of the first of `states` (the board's
/// state labels) it has ("" when it has none).
pub fn issue_ticket(i: &Value, states: &[String]) -> Ticket {
    let labels: Vec<String> = i["labels"].as_array().into_iter().flatten().map(|l| s(l, "name")).filter(|n| !n.is_empty()).collect();
    let n = i["number"].as_u64().unwrap_or(0);
    Ticket {
        id: n.to_string(),
        key: format!("#{n}"),
        title: s(i, "title"),
        description: s(i, "body"),
        url: s(i, "html_url"),
        column: labels.iter().find(|l| states.contains(l)).cloned().unwrap_or_default(),
        labels,
        assignees: i["assignees"].as_array().into_iter().flatten().map(|a| s(a, "login")).filter(|n| !n.is_empty()).collect(),
        comments: vec![],
        attachments: vec![],
    }
}

/// The issues a body's `Blocked by: #12, #13` lines name (in this repository: `owner/name#n` is
/// left out). Markdown around it is fine: `- **Blocked by:** #12`.
pub fn blocked_by(body: &str) -> Vec<u64> {
    let mut out = vec![];
    for line in body.lines() {
        let l = line.replace(['*', '_'], "");
        let l = l.trim_start_matches(|c: char| c.is_whitespace() || "-+>".contains(c)).to_lowercase();
        let Some(rest) = l.strip_prefix("blocked by") else { continue };
        let b = rest.as_bytes();
        for (i, _) in rest.match_indices('#') {
            let alone = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b"/-_.".contains(&b[i - 1]));
            let digits: String = rest[i + 1..].chars().take_while(char::is_ascii_digit).collect();
            if let (true, Ok(n)) = (alone, digits.parse::<u64>()) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// Saving a GitHub board: the token works, the repository is there and the token may write to it
/// (DevCat pushes branches, labels issues and opens pull requests as the token's owner). Sets the
/// owner's login and the branches' prefix `<login>/`, turns push on (a pull request needs the
/// branch on GitHub), and creates the board's state labels the repository doesn't have.
pub async fn on_save(b: &mut Board, token: String) -> AppResult<()> {
    if b.isolation == Isolation::InPlace && !b.target_branch.is_empty() {
        return Err(AppError::msg("A GitHub board opens a pull request for each issue, so each issue needs a branch of its own: choose a new branch per ticket"));
    }
    let tr = Tracker::new(b, token)?;
    let login = tr.gh_login().await?;
    let repo = json_of(tr.gh_repo(Method::GET, "").send().await?, "GitHub repository").await?;
    if repo["permissions"]["push"] == json!(false) {
        return Err(AppError::msg(format!("{login} can't write to {}: DevCat needs a token that can (issues, pull requests and contents: read and write)", b.project.trim())));
    }
    b.login = login.clone();
    b.branch_prefix = format!("{login}/");
    b.push = true;
    tr.gh_create_labels(&states(b)).await
}

/// What a fixed issue's draft pull request says: the agent's summary, how to test it, the line
/// that closes the issue when it merges, and who made it.
pub fn pr_body(t: &Ticket, rec: &RunRecord) -> String {
    let mut body = rec.summary.trim().to_string();
    if !rec.testing.trim().is_empty() {
        body.push_str(&format!("\n\n## How to test\n\n{}", rec.testing.trim()));
    }
    body.push_str(&format!("\n\nCloses {}\n\nOpened by the DevCat agent.", t.key));
    body
}

/// A fixed issue's pushed branch (`rec.branch`) becomes a draft pull request into the base
/// branch: the run's number and link (`rec.pr_number`, `rec.pr_url`), or a note when GitHub
/// refused it.
pub async fn pull_request(tr: &Tracker, t: &Ticket, rec: &mut RunRecord) {
    let Some(head) = rec.branch.clone() else { return };
    let made = async {
        let base = tr.gh_base().await?;
        tr.gh_pull_request(&head, &base, &t.title, &pr_body(t, rec)).await
    };
    match made.await {
        Ok((n, url)) => {
            rec.pr_number = Some(n);
            rec.pr_url = Some(url);
        }
        Err(e) => rec.notes.push(format!("Couldn't open the pull request: {e}")),
    }
}

/// Whether the watch may queue the issue: the last one to add the trigger label (the board's
/// pick-from label) may write to the repository or is on the board's allowlist, so a stranger's
/// label can't make the agent run; and no open issue blocks it, so it starts at the first check
/// after its last blocker closes. Sending an issue to the agent by hand skips this.
pub async fn ready(tr: &Tracker, t: &Ticket) -> AppResult<bool> {
    let label = tr.board.pick_from.as_ref().map(|c| c.id.clone()).unwrap_or_default();
    let Some(ev) = tr.gh_labeled(&t.id, &label).await? else { return Ok(false) };
    let who = s(&ev["actor"], "login");
    if !tr.board.allowlist.iter().any(|l| l.eq_ignore_ascii_case(&who)) && !can_write(tr, &who).await? {
        return Ok(false);
    }
    Ok(!tr.gh_blocked(t).await?)
}

/// Whether `login` may write to the board's repository (collaborator permission write, maintain
/// or admin). Asked once per login and tracker, i.e. per check or run.
pub(crate) async fn can_write(tr: &Tracker, login: &str) -> AppResult<bool> {
    let key = login.trim().to_lowercase();
    if key.is_empty() {
        return Ok(false);
    }
    if let Some(w) = tr.writers.lock().get(&key) {
        return Ok(*w);
    }
    let r = tr.gh_repo(Method::GET, &format!("/collaborators/{}/permission", enc(&key))).send().await?;
    // 404: no such user
    let w = if r.status().as_u16() == 404 {
        false
    } else {
        let v = json_of(r, "GitHub collaborator permission").await?;
        ["admin", "maintain", "write"].contains(&s(&v, "permission").as_str()) || v["user"]["permissions"]["push"] == json!(true)
    };
    tr.writers.lock().insert(key, w);
    Ok(w)
}

impl Tracker {
    // ------------------------------------------------------------ requests

    /// A request to `path` below the API address, with the token.
    fn gh(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", api(&self.board)))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// A request to `path` below the board's repository (`/repos/owner/name`).
    fn gh_repo(&self, method: Method, path: &str) -> RequestBuilder {
        self.gh(method, &format!("/repos/{}{path}", self.board.project.trim()))
    }

    /// The token owner's login.
    async fn gh_login(&self) -> AppResult<String> {
        let v = json_of(self.gh(Method::GET, "/user").send().await?, "GitHub user").await?;
        Some(s(&v, "login")).filter(|l| !l.is_empty()).ok_or_else(|| AppError::msg("GitHub didn't say whose token this is"))
    }

    /// The board's login (read when it was saved), else asked.
    async fn gh_me(&self) -> AppResult<String> {
        match self.board.login.trim() {
            "" => self.gh_login().await,
            l => Ok(l.to_string()),
        }
    }

    /// Where pull requests go: the base branch of a worktree board, else the repository's default branch.
    async fn gh_base(&self) -> AppResult<String> {
        if self.board.isolation == Isolation::Worktree && !self.board.target_branch.trim().is_empty() {
            return Ok(self.board.target_branch.trim().to_string());
        }
        let v = json_of(self.gh_repo(Method::GET, "").send().await?, "GitHub repository").await?;
        Some(s(&v, "default_branch")).filter(|b| !b.is_empty()).ok_or_else(|| AppError::msg("GitHub didn't say the repository's default branch"))
    }

    // ------------------------------------------------------------ labels

    /// The repository's labels, as columns (id and name are the label's name).
    pub(super) async fn gh_columns(&self) -> AppResult<Vec<Column>> {
        let mut out = vec![];
        for page in 1..=LABEL_PAGES {
            let r = self.gh_repo(Method::GET, "/labels").query(&[("per_page", "100".to_string()), ("page", page.to_string())]).send().await?;
            let v = json_of(r, "GitHub labels").await?;
            let got = v.as_array().map(Vec::len).unwrap_or(0);
            out.extend(v.as_array().into_iter().flatten().map(|l| Column { id: s(l, "name"), name: s(l, "name") }));
            if got < 100 {
                break;
            }
        }
        Ok(out)
    }

    /// Creates the labels the repository doesn't have yet.
    async fn gh_create_labels(&self, names: &[String]) -> AppResult<()> {
        let have: Vec<String> = self.gh_columns().await?.into_iter().map(|c| c.name.to_lowercase()).collect();
        for name in names.iter().filter(|n| !have.contains(&n.to_lowercase())) {
            let r = self.gh_repo(Method::POST, "/labels").json(&json!({ "name": name, "color": color(name), "description": "A state of DevCat's ticket agent" })).send().await?;
            // 422: made meanwhile
            if r.status().as_u16() != 422 {
                json_of(r, &format!("GitHub label {name}")).await?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ issues

    /// Open issues with `label` (pull requests left out), oldest first, filtered by assignee.
    pub(super) async fn gh_list(&self, label: &str) -> AppResult<Vec<Ticket>> {
        let assigned = self.board.assigned;
        let me = if assigned == Assigned::Anyone { String::new() } else { self.gh_me().await? };
        let mut q = vec![("state", "open"), ("labels", label), ("sort", "created"), ("direction", "asc"), ("per_page", "100")];
        if assigned == Assigned::Me {
            q.push(("assignee", &me));
        }
        let v = json_of(self.gh_repo(Method::GET, "/issues").query(&q).send().await?, "GitHub issues").await?;
        let states = states(&self.board);
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["pull_request"].is_null())
            .map(|i| issue_ticket(i, &states))
            .filter(|t| trello_wanted(&t.assignees, &me, assigned))
            .take(MAX_ISSUES)
            .collect())
    }

    /// One issue as it is now.
    pub(super) async fn gh_get(&self, id: &str) -> AppResult<Ticket> {
        let v = json_of(self.gh_repo(Method::GET, &format!("/issues/{id}")).send().await?, "GitHub issue").await?;
        if !v["pull_request"].is_null() {
            return Err(AppError::msg(format!("#{id} is a pull request, not an issue")));
        }
        Ok(issue_ticket(&v, &states(&self.board)))
    }

    /// The issue's latest `labeled` event for `label` (events come oldest first), if any.
    async fn gh_labeled(&self, id: &str, label: &str) -> AppResult<Option<Value>> {
        let mut last = None;
        for page in 1..=EVENT_PAGES {
            let r = self.gh_repo(Method::GET, &format!("/issues/{id}/events")).query(&[("per_page", "100".to_string()), ("page", page.to_string())]).send().await?;
            let v = json_of(r, "GitHub issue events").await?;
            let list = v.as_array().cloned().unwrap_or_default();
            if let Some(e) = list.iter().rev().find(|e| s(e, "event") == "labeled" && s(&e["label"], "name").eq_ignore_ascii_case(label)) {
                last = Some(e.clone());
            }
            if list.len() < 100 {
                break;
            }
        }
        Ok(last)
    }

    /// Whether an open issue blocks this one: in GitHub's issue dependencies (blockers may be in
    /// other repositories) or in a `Blocked by:` line of its body.
    async fn gh_blocked(&self, t: &Ticket) -> AppResult<bool> {
        let r = self.gh_repo(Method::GET, &format!("/issues/{}/dependencies/blocked_by", t.id)).query(&[("per_page", "100")]).send().await?;
        // 404: a GitHub without issue dependencies (an older Enterprise Server)
        if r.status().as_u16() != 404 && json_of(r, "GitHub issue blockers").await?.as_array().into_iter().flatten().any(|i| s(i, "state") == "open") {
            return Ok(true);
        }
        for n in blocked_by(&t.description).into_iter().filter(|n| n.to_string() != t.id) {
            let r = self.gh_repo(Method::GET, &format!("/issues/{n}")).send().await?;
            // 404: no such issue, nothing to wait for
            if r.status().as_u16() != 404 && s(&json_of(r, &format!("GitHub issue #{n}")).await?, "state") == "open" {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Swaps the issue's state labels (as it has them now) for `label` ("": none), leaving its
    /// other labels alone.
    pub(super) async fn gh_move(&self, t: &Ticket, label: &str) -> AppResult<()> {
        let v = json_of(self.gh_repo(Method::GET, &format!("/issues/{}", t.id)).send().await?, "GitHub issue").await?;
        let now: Vec<String> = v["labels"].as_array().into_iter().flatten().map(|l| s(l, "name")).collect();
        let states = states(&self.board);
        for l in now.iter().filter(|l| l.as_str() != label && (states.contains(l) || **l == t.column)) {
            let r = self.gh_repo(Method::DELETE, &format!("/issues/{}/labels/{}", t.id, enc(l))).send().await?;
            // 404: gone already
            if r.status().as_u16() != 404 {
                json_of(r, &format!("GitHub label {l}")).await?;
            }
        }
        if !label.is_empty() && !now.iter().any(|l| l == label) {
            json_of(self.gh_repo(Method::POST, &format!("/issues/{}/labels", t.id)).json(&json!({ "labels": [label] })).send().await?, &format!("GitHub label {label}")).await?;
        }
        Ok(())
    }

    pub(super) async fn gh_comment(&self, t: &Ticket, text: &str) -> AppResult<()> {
        json_of(self.gh_repo(Method::POST, &format!("/issues/{}/comments", t.id)).json(&json!({ "body": text })).send().await?, "GitHub comment").await.map(drop)
    }

    // ------------------------------------------------------------ pull requests

    /// A draft pull request from `head` into `base` (the open one for `head` when there is one
    /// already, e.g. an earlier run's): its number and link. A repository without draft pull
    /// requests (a private one on GitHub Free) gets a ready one.
    async fn gh_pull_request(&self, head: &str, base: &str, title: &str, body: &str) -> AppResult<(u64, String)> {
        let mut draft = true;
        loop {
            let r = self.gh_repo(Method::POST, "/pulls").json(&json!({ "title": title, "head": head, "base": base, "body": body, "draft": draft })).send().await?;
            if r.status().as_u16() != 422 {
                let v = json_of(r, "GitHub pull request").await?;
                return Ok((v["number"].as_u64().unwrap_or(0), s(&v, "html_url")));
            }
            let why = r.text().await.unwrap_or_default();
            if draft && why.contains("Draft pull requests are not supported") {
                draft = false;
                continue;
            }
            let owner = self.board.project.trim().split('/').next().unwrap_or_default().to_string();
            let open = json_of(self.gh_repo(Method::GET, "/pulls").query(&[("head", format!("{owner}:{head}")), ("state", "open".into())]).send().await?, "GitHub pull requests").await?;
            return match open.as_array().and_then(|a| a.first()) {
                Some(p) => Ok((p["number"].as_u64().unwrap_or(0), s(p, "html_url"))),
                None => Err(AppError::msg(format!("GitHub pull request: HTTP 422: {}", why.chars().take(300).collect::<String>()))),
            };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tickets::mock_github::{Gh, MockGithub};

    #[test]
    fn parses_issues() {
        let states = vec!["ready-for-agent".to_string(), "agent-working".to_string()];
        let t = issue_ticket(
            &json!({ "number": 7, "title": "Fix it", "body": null, "html_url": "https://github.com/acme/calc/issues/7",
                "labels": [{ "name": "bug" }, { "name": "agent-working" }, { "name": "ready-for-agent" }], "assignees": [{ "login": "octo" }] }),
            &states,
        );
        assert_eq!((t.id.as_str(), t.key.as_str(), t.title.as_str(), t.description.as_str()), ("7", "#7", "Fix it", ""));
        assert_eq!((t.url.as_str(), t.column.as_str()), ("https://github.com/acme/calc/issues/7", "agent-working"), "the first state label it has");
        assert_eq!((t.labels.len(), t.assignees.clone()), (3, vec!["octo".to_string()]));
        assert_eq!(issue_ticket(&json!({ "number": 8, "labels": [{ "name": "bug" }] }), &states).column, "", "no state label");
    }

    /// A GitHub board on `site` for `acme/calc` with the default state labels, a worktree for each
    /// ticket and `provider` as its agent, working in `repo`. A new id each time (boards.json is
    /// shared by the tests).
    pub(crate) fn board(site: &str, repo: &str, provider: &str) -> Board {
        let col = |n: &str| json!({ "id": n, "name": n });
        serde_json::from_value(json!({ "id": format!("gh-{}", uuid::Uuid::new_v4()), "name": "", "kind": "github", "site": site, "project": "acme/calc", "token": "t",
            "repo": repo, "providerId": provider, "isolation": "worktree", "assigned": "me",
            "pickFrom": col("ready-for-agent"), "working": col("agent-working"), "doneTo": col("agent-review"), "failedTo": col("ready-for-human") }))
        .unwrap()
    }

    /// Boards can be saved and read (the tests share one data folder).
    pub(crate) fn tickets_data() {
        super::super::init_reading(std::env::temp_dir().join(format!("devcat-tickets-data-{}", std::process::id())));
    }

    /// Saving checks the token and the repository, reads the token owner's login (the branches'
    /// prefix), turns push on and creates the state labels the repository lacks. A bad token, a
    /// repository that isn't there and a token that can't write to it are refused.
    #[tokio::test]
    async fn saving_a_board() {
        tickets_data();
        let gh = MockGithub::start(Gh::new("octo", "acme/calc")).await;
        let saved = super::super::tickets_board_save(Board { push: false, ..board(&gh.url, "", "") }).await.unwrap();
        assert_eq!((saved.login.as_str(), saved.branch_prefix.as_str(), saved.push, saved.name.as_str()), ("octo", "octo/", true, "GitHub acme/calc"));
        assert!(saved.has_token);
        assert_eq!(gh.gh.lock().labels, vec!["bug", "ready-for-agent", "agent-working", "agent-review", "ready-for-human"]);
        assert_eq!(gh.sent("POST /repos/acme/calc/labels").len(), 3, "only the missing ones: {:?}", gh.requests());
        assert!(gh.requests().iter().any(|r| r == "GET /user") && gh.requests().iter().any(|r| r == "GET /repos/acme/calc"), "{:?}", gh.requests());
        let cols = super::super::tickets_columns(saved.clone()).await.unwrap();
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["bug", "ready-for-agent", "agent-working", "agent-review", "ready-for-human"], "the columns are the labels");

        let refused = |b: Board| async move { super::super::tickets_board_save(b).await.unwrap_err().to_string() };
        let e = refused(Board { project: "acme/nope".into(), ..board(&gh.url, "", "") }).await;
        assert!(e.contains("404"), "{e}");
        assert!(refused(Board { project: "calc".into(), ..board(&gh.url, "", "") }).await.contains("owner/name"));
        gh.gh.lock().push = false;
        assert!(refused(board(&gh.url, "", "")).await.contains("octo can't write to acme/calc"));
        gh.gh.lock().canned.push(("GET /user".into(), 401, json!({ "message": "Bad credentials" })));
        let e = refused(board(&gh.url, "", "")).await;
        assert!(e.contains("401") && e.contains("check the token"), "{e}");
    }

    /// Listing a label gives its open issues, not pull requests or closed ones, filtered by the
    /// board's assignee setting.
    #[tokio::test]
    async fn listing_issues() {
        tickets_data();
        let mut data = Gh::new("octo", "acme/calc")
            .with_issue(7, "Mine", "", &["bug", "ready-for-agent"], &["octo"])
            .with_issue(8, "Someone else's", "", &["ready-for-agent"], &["ana"])
            .with_issue(9, "A pull request", "", &["ready-for-agent"], &["octo"])
            .with_issue(10, "Nobody's", "", &["ready-for-agent"], &[])
            .with_issue(11, "Closed", "", &["ready-for-agent"], &["octo"])
            .with_issue(12, "Not labelled", "", &["bug"], &["octo"]);
        data.issues.get_mut(&9).unwrap()["pull_request"] = json!({ "url": "https://api.github.com/repos/acme/calc/pulls/9" });
        data.issues.get_mut(&11).unwrap()["state"] = json!("closed");
        let gh = MockGithub::start(data).await;
        let list = |assigned: &str| {
            let b = Board { assigned: serde_json::from_value(json!(assigned)).unwrap(), ..board(&gh.url, "", "") };
            async move {
                let b = super::super::tickets_board_save(b).await.unwrap();
                super::super::tickets_list(b.id, "ready-for-agent".into()).await.unwrap().into_iter().map(|t| t.key).collect::<Vec<_>>()
            }
        };
        assert_eq!(list("me").await, vec!["#7"]);
        let asked = gh.sent("GET /repos/acme/calc/issues?");
        assert!(asked.last().is_some_and(|r| r.contains("state=open") && r.contains("labels=ready-for-agent") && r.contains("assignee=octo")), "{asked:?}");
        assert_eq!(list("meOrNobody").await, vec!["#7", "#10"]);
        assert_eq!(list("anyone").await, vec!["#7", "#8", "#10"]);
        let t = super::super::tickets_list(super::super::tickets_board_save(board(&gh.url, "", "")).await.unwrap().id, "ready-for-agent".into()).await.unwrap().remove(0);
        assert_eq!((t.id.as_str(), t.column.as_str(), t.url.as_str()), ("7", "ready-for-agent", "https://github.com/acme/calc/issues/7"));
    }

    /// A `labeled` event for `label` by `by` at `when` (RFC 3339), as GitHub lists it.
    fn labeled(label: &str, by: &str, when: &str) -> Value {
        json!({ "id": 1, "event": "labeled", "created_at": when, "actor": { "login": by }, "label": { "name": label, "color": "0E8A16" }, "performed_via_github_app": null })
    }

    /// A minute from now: a label added after the board was saved.
    fn soon() -> String {
        (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339()
    }

    /// What the watch's next check would queue.
    async fn queued(b: &Board) -> Vec<String> {
        super::super::worker::to_queue(b).await.unwrap()
    }

    /// The watch takes an issue only when the person who last added the trigger label may write
    /// to the repository (write, maintain or admin) or is on the board's allowlist; asking GitHub
    /// once per login and check.
    #[tokio::test]
    async fn trusted_label_actor() {
        tickets_data();
        let mut data = Gh::new("octo", "acme/calc")
            .with_issue(7, "By an admin", "", &["ready-for-agent"], &["octo"])
            .with_issue(8, "By a writer", "", &["ready-for-agent"], &["octo"])
            .with_issue(9, "By a reader", "", &["ready-for-agent"], &["octo"])
            .with_issue(10, "By a stranger", "", &["ready-for-agent"], &["octo"])
            .with_issue(11, "Writer's label taken off, stranger's put back", "", &["ready-for-agent"], &["octo"])
            .with_issue(12, "Admin again", "", &["ready-for-agent"], &["octo"]);
        for (n, who) in [(7, "ana"), (8, "bo"), (9, "cy"), (10, "eve"), (12, "ana")] {
            data.events.insert(n, vec![labeled("bug", "eve", &soon()), labeled("ready-for-agent", who, &soon())]);
        }
        data.events.insert(11, vec![labeled("ready-for-agent", "bo", &soon()), json!({ "event": "unlabeled", "created_at": soon(), "actor": { "login": "bo" }, "label": { "name": "ready-for-agent" } }), labeled("Ready-For-Agent", "eve", &soon())]);
        data.permissions = [("ana", "admin"), ("bo", "write"), ("cy", "read")].into_iter().map(|(l, p)| (l.to_string(), p.to_string())).collect();
        let gh = MockGithub::start(data).await;
        let b = super::super::tickets_board_save(Board { max_per_run: 20, ..board(&gh.url, "", "") }).await.unwrap();
        assert_eq!(queued(&b).await, vec!["7", "8", "12"]);
        assert_eq!(gh.sent("GET /repos/acme/calc/collaborators/ana/permission").len(), 1, "once per login: {:?}", gh.requests());

        let b = super::super::tickets_board_save(Board { allowlist: vec![" @Eve ".into(), "".into()], ..b }).await.unwrap();
        assert_eq!(b.allowlist, vec!["eve"]);
        assert_eq!(queued(&b).await, vec!["7", "8", "10", "11", "12"], "eve is on the allowlist");
        assert_eq!(queued(&Board { max_per_run: 2, ..b }).await, vec!["7", "8"], "at most max_per_run");
    }

    #[test]
    fn blocked_by_lines() {
        let body = "Fix it.\nBlocked by: #12, #13\n- **Blocked by** #14\n  blocked by:#15 and acme/other#16\nNot blocked by #17 here.\nBlocked by: nothing";
        assert_eq!(blocked_by(body), vec![12, 13, 14, 15]);
        assert!(blocked_by("").is_empty());
    }

    /// An issue with an open blocker, in GitHub's dependencies or in a `Blocked by:` line, waits;
    /// the next check after its last blocker closes takes it.
    #[tokio::test]
    async fn blocked_issues_wait() {
        tickets_data();
        let mut data = Gh::new("octo", "acme/calc")
            .with_issue(7, "Blocked on GitHub", "", &["ready-for-agent"], &["octo"])
            .with_issue(8, "Blocked in its body", "Do it.\n\nBlocked by: #21, #22", &["ready-for-agent"], &["octo"])
            .with_issue(9, "Its blockers are closed", "**Blocked by:** #23", &["ready-for-agent"], &["octo"])
            .with_issue(20, "Open blocker", "", &[], &[])
            .with_issue(21, "Closed blocker", "", &[], &[])
            .with_issue(22, "Open blocker", "", &[], &[])
            .with_issue(23, "Closed blocker", "", &[], &[]);
        for n in [21, 23] {
            data.issues.get_mut(&n).unwrap()["state"] = json!("closed");
        }
        data.blocked_by = [(7, vec![20, 21]), (9, vec![21])].into_iter().collect();
        for n in [7, 8, 9] {
            data.events.insert(n, vec![labeled("ready-for-agent", "ana", &soon())]);
        }
        data.permissions.insert("ana".into(), "write".into());
        let gh = MockGithub::start(data).await;
        let b = super::super::tickets_board_save(board(&gh.url, "", "")).await.unwrap();
        assert_eq!(queued(&b).await, vec!["9"]);
        gh.gh.lock().issues.get_mut(&20).unwrap()["state"] = json!("closed");
        assert_eq!(queued(&b).await, vec!["7", "9"], "#7's last blocker closed");
        gh.gh.lock().issues.get_mut(&22).unwrap()["state"] = json!("closed");
        assert_eq!(queued(&b).await, vec!["7", "8", "9"], "#8's last blocker closed");
    }

    /// The issue the run tests work on, and its branch.
    pub(crate) const ISSUE: &str = "average() gives the wrong result";
    pub(crate) const BRANCH: &str = "octo/7-average-gives-the-wrong-result";

    /// `Gh::new("octo", "acme/calc")` with issue #7 (`ISSUE`) labelled `ready-for-agent`,
    /// assigned to octo.
    pub(crate) fn one_issue() -> Gh {
        Gh::new("octo", "acme/calc").with_issue(7, ISSUE, "average([2, 4]) returns 6 instead of 3. Fix average in calc.py.", &["bug", "ready-for-agent"], &["octo"])
    }

    /// A fixed issue: the agent works on `<login>/<number>-<slug>`, DevCat pushes it, opens a
    /// draft pull request into the default branch that closes the issue, comments with the summary
    /// and the link, and the issue keeps exactly one state label: agent-working during the run,
    /// agent-review after it.
    #[cfg(unix)]
    #[tokio::test]
    async fn fixed_issue_gets_a_draft_pull_request() {
        use super::super::worker::{fix_one, git, tests::*};
        tickets_data();
        let gh = MockGithub::start(one_issue()).await;
        let (dir, remote) = clone_with_origin("gh-run").await;
        let (cwd, origin) = (dir.to_str().unwrap(), remote.to_str().unwrap());
        let (fixer, _) = stand_in("fixed", "echo '    # fixed' >> calc.py").await;
        let b = super::super::tickets_board_save(board(&gh.url, cwd, &fixer)).await.unwrap();
        let rec = fix_one(&b, "7", None, None, &devcat_harness::Cancel::default()).await;
        assert_eq!(rec.outcome, "fixed", "{} {:?}", rec.summary, rec.notes);
        assert_eq!((rec.branch.as_deref(), rec.moved_to.as_deref()), (Some(BRANCH), Some("agent-review")));
        assert_eq!(git(origin, &["rev-parse", BRANCH]).await.unwrap(), git(cwd, &["rev-parse", BRANCH]).await.unwrap(), "pushed");
        assert!(git(cwd, &["show", &format!("{BRANCH}:calc.py")]).await.unwrap().contains("# fixed"));
        assert!(worktrees(cwd).await.is_empty());

        assert_eq!(gh.labels_of(7), vec!["bug", "agent-review"], "one state label: {:?}", gh.requests());
        assert!(gh.sent("POST /repos/acme/calc/issues/7/labels").first().is_some_and(|r| r.ends_with(r#"{"labels":["agent-working"]}"#)), "{:?}", gh.requests());
        assert_eq!(gh.sent("DELETE /repos/acme/calc/issues/7/labels/").len(), 2, "ready-for-agent, then agent-working: {:?}", gh.requests());

        let pulls: Vec<serde_json::Value> = gh.gh.lock().pulls.values().cloned().collect();
        assert_eq!(pulls.len(), 1, "{:?}", gh.requests());
        let pr = &pulls[0];
        assert_eq!((pr["draft"].clone(), pr["base"]["ref"].as_str(), pr["head"]["ref"].as_str(), pr["title"].as_str()), (json!(true), Some("main"), Some(BRANCH), Some(ISSUE)));
        let body = pr["body"].as_str().unwrap();
        assert!(body.contains("Stand-in summary.") && body.contains("Closes #7") && body.contains("DevCat"), "{body}");
        assert_eq!((rec.pr_number, rec.pr_url.as_deref()), (pr["number"].as_u64(), pr["html_url"].as_str()));

        let comments = gh.comments_on(7);
        assert_eq!(comments.len(), 1, "{comments:?}");
        assert!(comments[0].contains("Stand-in summary.") && comments[0].contains(pr["html_url"].as_str().unwrap()), "{}", comments[0]);

        // Fixed again (on the same branch): the open pull request is the run's.
        let (again, _) = stand_in("fixed", "echo '    # again' >> calc.py").await;
        let rec = fix_one(&Board { provider_id: again, ..b }, "7", None, None, &devcat_harness::Cancel::default()).await;
        assert_eq!((rec.outcome.as_str(), rec.pr_number), ("fixed", pr["number"].as_u64()), "{} {:?}", rec.summary, rec.notes);
        assert_eq!(gh.gh.lock().pulls.len(), 1);
        assert!(git(origin, &["show", &format!("{BRANCH}:calc.py")]).await.unwrap().contains("# again"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    /// The whole GitHub flow with real Claude Code (haiku; costs a few cents): the issue is fixed
    /// in its worktree, pushed, and gets a draft pull request.
    #[tokio::test]
    #[ignore]
    async fn real_github_run() {
        use super::super::worker::{fix_one, git, tests::*};
        crate::ai::init(std::env::temp_dir().join(format!("devcat-tickets-ai-{}", std::process::id())));
        tickets_data();
        let gh = MockGithub::start(one_issue()).await;
        let (dir, remote) = clone_with_origin("gh-real").await;
        let cwd = dir.to_str().unwrap();
        let b = super::super::tickets_board_save(Board { model: Some("haiku".into()), ..board(&gh.url, cwd, "claude") }).await.unwrap();
        let rec = fix_one(&b, "7", None, None, &devcat_harness::Cancel::default()).await;
        println!("{}", serde_json::to_string_pretty(&rec).unwrap());
        assert_eq!(rec.outcome, "fixed", "{} {:?}", rec.summary, rec.notes);
        assert!(git(remote.to_str().unwrap(), &["show", &format!("{BRANCH}:calc.py")]).await.unwrap().contains("len(xs)"));
        assert_eq!(gh.gh.lock().pulls.len(), 1, "{:?}", gh.requests());
        assert_eq!(gh.labels_of(7), vec!["bug", "agent-review"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }
}
