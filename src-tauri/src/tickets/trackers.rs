//! The trackers' APIs: list a board's columns, the tickets in one, read a ticket with its recent
//! comments, move it to another column and comment on it.
//!
//! - Jira (Cloud: email + API token; Server / Data Center: a personal access token). Columns are
//!   the project's statuses; a ticket moves through the workflow transition that leads to the
//!   status (there may be none from where it is). REST API v2: plain-text descriptions and
//!   comments, and it exists on both Cloud and Data Center.
//! - Trello (API key + token). Columns are the board's lists.
//! - Linear (personal API key). Columns are the team's workflow states.
use super::{Assigned, Board, Column, Comment, Ticket, TrackerKind};
use crate::error::{AppError, AppResult};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

/// Tickets read from one column, at most.
const MAX_TICKETS: usize = 50;
/// Recent comments given to the agent.
const MAX_COMMENTS: usize = 10;
const LINEAR_API: &str = "https://api.linear.app/graphql";
const TRELLO_API: &str = "https://api.trello.com/1";

pub struct Tracker {
    http: Client,
    board: Board,
    token: String,
}

fn http() -> AppResult<Client> {
    Ok(Client::builder().timeout(Duration::from_secs(30)).user_agent(concat!("DevCat/", env!("CARGO_PKG_VERSION"))).build()?)
}

/// The response's JSON, or an error with the status and the start of the body.
async fn json_of(r: Response, what: &str) -> AppResult<Value> {
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => " (check the token)",
            403 => " (the token can't do this)",
            404 => " (check the site, project or board)",
            _ => "",
        };
        let body: String = text.chars().take(300).collect();
        return Err(AppError::msg(format!("{what}: HTTP {status}{hint}: {body}")));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| AppError::msg(format!("{what}: unexpected answer ({e})")))
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// JQL for a project's tickets in one status, filtered by assignee.
pub fn jira_jql(project: &str, status: &str, assigned: Assigned) -> String {
    let who = match assigned {
        Assigned::Anyone => "",
        Assigned::Me => " AND assignee = currentUser()",
        Assigned::MeOrNobody => " AND (assignee = currentUser() OR assignee is EMPTY)",
    };
    format!("project = \"{}\" AND status = {}{who} ORDER BY created ASC", project.replace('"', ""), status.trim().replace(|c: char| !c.is_ascii_digit(), ""))
}

/// Linear's IssueFilter for a team's issues in one state, filtered by assignee.
pub fn linear_filter(team: &str, state: &str, assigned: Assigned) -> Value {
    let mut f = json!({ "team": { "key": { "eq": team } }, "state": { "id": { "eq": state } } });
    match assigned {
        Assigned::Anyone => {}
        Assigned::Me => f["assignee"] = json!({ "isMe": { "eq": true } }),
        Assigned::MeOrNobody => f["or"] = json!([{ "assignee": { "isMe": { "eq": true } } }, { "assignee": { "null": true } }]),
    }
    f
}

/// Whether a Trello card with these members is wanted (`me`: the token's member id).
pub fn trello_wanted(members: &[String], me: &str, assigned: Assigned) -> bool {
    match assigned {
        Assigned::Anyone => true,
        Assigned::Me => members.iter().any(|m| m == me),
        Assigned::MeOrNobody => members.is_empty() || members.iter().any(|m| m == me),
    }
}

/// Card fields DevCat reads, with the members' names.
const TRELLO_CARD: [(&str, &str); 3] = [("fields", "name,desc,url,idShort,idList,labels,idMembers"), ("members", "true"), ("member_fields", "fullName")];

/// `https://trello.com/b/AbC123/name` -> `AbC123` (an id or short link is kept as it is).
pub fn trello_board_id(board: &str) -> String {
    let b = board.trim().trim_end_matches('/');
    match b.find("/b/") {
        Some(i) => b[i + 3..].split('/').next().unwrap_or_default().to_string(),
        None => b.to_string(),
    }
}

impl Tracker {
    pub fn new(board: &Board, token: String) -> AppResult<Tracker> {
        if token.trim().is_empty() {
            return Err(AppError::msg("No token saved for this board"));
        }
        if board.project.trim().is_empty() {
            return Err(AppError::msg(match board.kind {
                TrackerKind::Jira => "Enter the Jira project key",
                TrackerKind::Trello => "Enter the Trello board (its link or id)",
                TrackerKind::Linear => "Enter the Linear team key",
            }));
        }
        if board.kind == TrackerKind::Jira && !board.site.trim().starts_with("http") {
            return Err(AppError::msg("Enter the Jira site, e.g. https://acme.atlassian.net"));
        }
        Ok(Tracker { http: http()?, board: board.clone(), token: token.trim().to_string() })
    }

    // ------------------------------------------------------------ requests

    fn jira(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let url = format!("{}/rest/api/2{path}", self.board.site.trim().trim_end_matches('/'));
        let rb = self.http.request(method, url).header("Accept", "application/json");
        if self.board.user.trim().is_empty() { rb.bearer_auth(&self.token) } else { rb.basic_auth(self.board.user.trim(), Some(&self.token)) }
    }

    fn trello(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{TRELLO_API}{path}")).query(&[("key", self.board.user.trim()), ("token", &self.token)])
    }

    async fn linear(&self, query: &str, variables: Value) -> AppResult<Value> {
        let r = self.http.post(LINEAR_API).header("Authorization", &self.token).json(&json!({ "query": query, "variables": variables })).send().await?;
        let v = json_of(r, "Linear").await?;
        if let Some(e) = v["errors"].as_array().and_then(|e| e.first()) {
            return Err(AppError::msg(format!("Linear: {}", s(e, "message"))));
        }
        Ok(v["data"].clone())
    }

    fn project(&self) -> String {
        self.board.project.trim().to_string()
    }

    // ------------------------------------------------------------ columns

    pub async fn columns(&self) -> AppResult<Vec<Column>> {
        match self.board.kind {
            TrackerKind::Jira => {
                let r = self.jira(reqwest::Method::GET, &format!("/project/{}/statuses", self.project())).send().await?;
                let v = json_of(r, "Jira statuses").await?;
                let mut seen = HashSet::new();
                let mut out = vec![];
                for t in v.as_array().into_iter().flatten() {
                    for st in t["statuses"].as_array().into_iter().flatten() {
                        let id = s(st, "id");
                        if seen.insert(id.clone()) {
                            out.push(Column { id, name: s(st, "name") });
                        }
                    }
                }
                Ok(out)
            }
            TrackerKind::Trello => {
                let r = self.trello(reqwest::Method::GET, &format!("/boards/{}/lists", trello_board_id(&self.board.project))).query(&[("filter", "open")]).send().await?;
                let v = json_of(r, "Trello lists").await?;
                Ok(v.as_array().into_iter().flatten().map(|l| Column { id: s(l, "id"), name: s(l, "name") }).collect())
            }
            TrackerKind::Linear => {
                let d = self
                    .linear("query($k:String!){teams(filter:{key:{eq:$k}}){nodes{states{nodes{id name position}}}}}", json!({ "k": self.project() }))
                    .await?;
                let team = d["teams"]["nodes"].as_array().and_then(|t| t.first()).ok_or_else(|| AppError::msg(format!("Linear: no team with the key {}", self.project())))?;
                let mut states: Vec<&Value> = team["states"]["nodes"].as_array().into_iter().flatten().collect();
                states.sort_by(|a, b| a["position"].as_f64().unwrap_or(0.0).total_cmp(&b["position"].as_f64().unwrap_or(0.0)));
                Ok(states.into_iter().map(|st| Column { id: s(st, "id"), name: s(st, "name") }).collect())
            }
        }
    }

    // ------------------------------------------------------------ tickets

    /// Tickets in a column (without comments), oldest first where the tracker allows.
    pub async fn list(&self, column: &str) -> AppResult<Vec<Ticket>> {
        match self.board.kind {
            TrackerKind::Jira => {
                let jql = jira_jql(&self.project(), column, self.board.assigned);
                let q = [("jql", jql.as_str()), ("fields", "summary,description,labels,status,assignee"), ("maxResults", "50")];
                // Cloud moved search to /search/jql; Data Center only has /search.
                let mut r = self.jira(reqwest::Method::GET, "/search/jql").query(&q).send().await?;
                if matches!(r.status().as_u16(), 404 | 405 | 410) {
                    r = self.jira(reqwest::Method::GET, "/search").query(&q).send().await?;
                }
                let v = json_of(r, "Jira search").await?;
                Ok(v["issues"].as_array().into_iter().flatten().take(MAX_TICKETS).map(|i| self.jira_ticket(i)).collect())
            }
            TrackerKind::Trello => {
                let me = match self.board.assigned {
                    Assigned::Anyone => String::new(),
                    _ => {
                        let r = self.trello(reqwest::Method::GET, "/members/me").query(&[("fields", "id")]).send().await?;
                        s(&json_of(r, "Trello member").await?, "id")
                    }
                };
                let r = self.trello(reqwest::Method::GET, &format!("/lists/{column}/cards")).query(&TRELLO_CARD).send().await?;
                let v = json_of(r, "Trello cards").await?;
                Ok(v.as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| {
                        let ids: Vec<String> = c["idMembers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                        trello_wanted(&ids, &me, self.board.assigned)
                    })
                    .take(MAX_TICKETS)
                    .map(trello_ticket)
                    .collect())
            }
            TrackerKind::Linear => {
                let d = self
                    .linear(
                        "query($f:IssueFilter){issues(first:50,filter:$f){nodes{id identifier title description url state{id} labels{nodes{name}} assignee{name}}}}",
                        json!({ "f": linear_filter(&self.project(), column, self.board.assigned) }),
                    )
                    .await?;
                let mut out: Vec<Ticket> = d["issues"]["nodes"].as_array().into_iter().flatten().map(linear_ticket).collect();
                out.reverse(); // newest first from the API
                Ok(out)
            }
        }
    }

    /// One ticket as it is now, with its recent comments.
    pub async fn get(&self, id: &str) -> AppResult<Ticket> {
        match self.board.kind {
            TrackerKind::Jira => {
                let r = self.jira(reqwest::Method::GET, &format!("/issue/{id}")).query(&[("fields", "summary,description,labels,status,assignee,comment")]).send().await?;
                let v = json_of(r, "Jira issue").await?;
                let mut t = self.jira_ticket(&v);
                let all: Vec<&Value> = v["fields"]["comment"]["comments"].as_array().into_iter().flatten().collect();
                t.comments = all.iter().rev().take(MAX_COMMENTS).rev().map(|c| Comment { author: s(&c["author"], "displayName"), text: s(c, "body") }).collect();
                Ok(t)
            }
            TrackerKind::Trello => {
                let r = self.trello(reqwest::Method::GET, &format!("/cards/{id}")).query(&TRELLO_CARD).send().await?;
                let mut t = trello_ticket(&json_of(r, "Trello card").await?);
                let r = self.trello(reqwest::Method::GET, &format!("/cards/{id}/actions")).query(&[("filter", "commentCard"), ("limit", "10")]).send().await?;
                let v = json_of(r, "Trello comments").await?;
                // newest first from the API
                t.comments = v.as_array().into_iter().flatten().rev().map(|a| Comment { author: s(&a["memberCreator"], "fullName"), text: s(&a["data"], "text") }).collect();
                Ok(t)
            }
            TrackerKind::Linear => {
                let d = self
                    .linear(
                        "query($id:String!){issue(id:$id){id identifier title description url state{id} labels{nodes{name}} assignee{name} comments(last:10){nodes{body createdAt user{name}}}}}",
                        json!({ "id": id }),
                    )
                    .await?;
                let i = &d["issue"];
                let mut t = linear_ticket(i);
                let mut cs: Vec<&Value> = i["comments"]["nodes"].as_array().into_iter().flatten().collect();
                cs.sort_by_key(|c| s(c, "createdAt"));
                t.comments = cs.into_iter().map(|c| Comment { author: s(&c["user"], "name"), text: s(c, "body") }).collect();
                Ok(t)
            }
        }
    }

    fn jira_ticket(&self, i: &Value) -> Ticket {
        let key = s(i, "key");
        let f = &i["fields"];
        Ticket {
            url: format!("{}/browse/{key}", self.board.site.trim().trim_end_matches('/')),
            id: key.clone(),
            key,
            title: s(f, "summary"),
            description: s(f, "description"),
            labels: f["labels"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
            assignees: Some(s(&f["assignee"], "displayName")).filter(|n| !n.is_empty()).into_iter().collect(),
            column: s(&f["status"], "id"),
            comments: vec![],
        }
    }

    // ------------------------------------------------------------ changes

    pub async fn move_to(&self, t: &Ticket, column: &str) -> AppResult<()> {
        if t.column == column {
            return Ok(());
        }
        match self.board.kind {
            TrackerKind::Jira => {
                let r = self.jira(reqwest::Method::GET, &format!("/issue/{}/transitions", t.id)).send().await?;
                let v = json_of(r, "Jira transitions").await?;
                let tr = v["transitions"].as_array().into_iter().flatten().find(|x| s(&x["to"], "id") == column).ok_or_else(|| {
                    AppError::msg(format!("Jira's workflow has no transition that takes {} to that status from where it is", t.key))
                })?;
                let r = self.jira(reqwest::Method::POST, &format!("/issue/{}/transitions", t.id)).json(&json!({ "transition": { "id": s(tr, "id") } })).send().await?;
                json_of(r, "Jira transition").await.map(drop)
            }
            TrackerKind::Trello => {
                let r = self.trello(reqwest::Method::PUT, &format!("/cards/{}", t.id)).query(&[("idList", column), ("pos", "top")]).send().await?;
                json_of(r, "Trello move").await.map(drop)
            }
            TrackerKind::Linear => {
                let d = self.linear("mutation($id:String!,$s:String!){issueUpdate(id:$id,input:{stateId:$s}){success}}", json!({ "id": t.id, "s": column })).await?;
                if d["issueUpdate"]["success"] != json!(true) {
                    return Err(AppError::msg("Linear didn't move the issue"));
                }
                Ok(())
            }
        }
    }

    pub async fn comment(&self, t: &Ticket, text: &str) -> AppResult<()> {
        match self.board.kind {
            TrackerKind::Jira => {
                let r = self.jira(reqwest::Method::POST, &format!("/issue/{}/comment", t.id)).json(&json!({ "body": text })).send().await?;
                json_of(r, "Jira comment").await.map(drop)
            }
            TrackerKind::Trello => {
                let r = self.trello(reqwest::Method::POST, &format!("/cards/{}/actions/comments", t.id)).query(&[("text", text)]).send().await?;
                json_of(r, "Trello comment").await.map(drop)
            }
            TrackerKind::Linear => {
                let d = self.linear("mutation($id:String!,$b:String!){commentCreate(input:{issueId:$id,body:$b}){success}}", json!({ "id": t.id, "b": text })).await?;
                if d["commentCreate"]["success"] != json!(true) {
                    return Err(AppError::msg("Linear didn't add the comment"));
                }
                Ok(())
            }
        }
    }
}

fn trello_ticket(c: &Value) -> Ticket {
    Ticket {
        id: s(c, "id"),
        key: format!("#{}", c["idShort"].as_u64().unwrap_or(0)),
        title: s(c, "name"),
        description: s(c, "desc"),
        url: s(c, "url"),
        labels: c["labels"].as_array().into_iter().flatten().map(|l| s(l, "name")).filter(|n| !n.is_empty()).collect(),
        assignees: c["members"].as_array().into_iter().flatten().map(|m| s(m, "fullName")).filter(|n| !n.is_empty()).collect(),
        column: s(c, "idList"),
        comments: vec![],
    }
}

fn linear_ticket(i: &Value) -> Ticket {
    Ticket {
        id: s(i, "id"),
        key: s(i, "identifier"),
        title: s(i, "title"),
        description: s(i, "description"),
        url: s(i, "url"),
        labels: i["labels"]["nodes"].as_array().into_iter().flatten().map(|l| s(l, "name")).collect(),
        assignees: Some(s(&i["assignee"], "name")).filter(|n| !n.is_empty()).into_iter().collect(),
        column: s(&i["state"], "id"),
        comments: vec![],
    }
}

/// Post `text` to a chat webhook (Slack incoming webhook; Mattermost and Google Chat take the same).
pub async fn post_webhook(url: &str, text: &str) -> AppResult<()> {
    if !url.starts_with("https://") {
        return Err(AppError::msg("The webhook must be an https:// URL"));
    }
    let r = http()?.post(url).json(&json!({ "text": text })).send().await?;
    // Slack answers "ok" (not JSON): only the status matters.
    let status = r.status();
    if !status.is_success() {
        let body: String = r.text().await.unwrap_or_default().chars().take(200).collect();
        return Err(AppError::msg(format!("Webhook: HTTP {status}: {body}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trello_ids() {
        assert_eq!(trello_board_id("https://trello.com/b/AbC123/my-board"), "AbC123");
        assert_eq!(trello_board_id("https://trello.com/b/AbC123/"), "AbC123");
        assert_eq!(trello_board_id("AbC123"), "AbC123");
    }

    #[test]
    fn assignee_filters() {
        assert_eq!(jira_jql("ABC", "10001", Assigned::Anyone), "project = \"ABC\" AND status = 10001 ORDER BY created ASC");
        assert!(jira_jql("ABC", "1", Assigned::Me).contains(" AND assignee = currentUser() ORDER"));
        assert!(jira_jql("ABC", "1", Assigned::MeOrNobody).contains("(assignee = currentUser() OR assignee is EMPTY)"));
        assert!(linear_filter("ENG", "s", Assigned::Anyone).get("assignee").is_none());
        assert_eq!(linear_filter("ENG", "s", Assigned::Me)["assignee"], json!({ "isMe": { "eq": true } }));
        assert_eq!(linear_filter("ENG", "s", Assigned::MeOrNobody)["or"][1], json!({ "assignee": { "null": true } }));
        let (me, other) = ("m1".to_string(), "m2".to_string());
        assert!(trello_wanted(&[other.clone()], &me, Assigned::Anyone));
        assert!(!trello_wanted(&[other.clone()], &me, Assigned::Me));
        assert!(trello_wanted(&[other, me.clone()], &me, Assigned::Me));
        assert!(!trello_wanted(&[], &me, Assigned::Me));
        assert!(trello_wanted(&[], &me, Assigned::MeOrNobody));
    }

    #[test]
    fn parses_tickets() {
        let t = trello_ticket(&json!({ "id": "c1", "idShort": 7, "name": "Fix", "desc": "d", "url": "u", "idList": "l1", "labels": [{ "name": "bug" }, { "name": "" }], "members": [{ "fullName": "Ana" }] }));
        assert_eq!((t.key.as_str(), t.column.as_str(), t.labels.clone(), t.assignees.clone()), ("#7", "l1", vec!["bug".to_string()], vec!["Ana".to_string()]));
        let t = linear_ticket(&json!({ "id": "i1", "identifier": "ENG-4", "title": "T", "description": null, "url": "u", "state": { "id": "s1" }, "labels": { "nodes": [] } }));
        assert_eq!((t.key.as_str(), t.description.as_str(), t.column.as_str()), ("ENG-4", "", "s1"));
    }
}
