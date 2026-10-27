# Tickets

Connect a Jira project, Trello board or Linear team and let your coding agent work through its tickets. You
choose the column it takes tickets from (say, **To Do**). For each ticket the agent fixes it on its own git
branch, DevCat commits the work, comments on the ticket with what changed and how to test it, and moves it on
(say, to **Testing**). Each result can also be posted to a Slack channel.

## Connect a board

Click **Tickets** in the left bar (the checklist icon), then **Connect a board** (or **+** at the top of the
list). The list shows each board and what the agent is doing on it; click one to see its tickets.

| Tracker | What you enter | Token |
| --- | --- | --- |
| Jira Cloud | Site (`https://acme.atlassian.net`), your email, project key (`ABC`) | [API token](https://id.atlassian.com/manage-profile/security/api-tokens) |
| Jira Data Center / Server | Site, project key; leave **Email** empty | Personal access token |
| Trello | API key, board link (`https://trello.com/b/…`) | [Token](https://trello.com/power-ups/admin) for that key |
| Linear | Team key (`ENG` for `ENG-123`) | [Personal API key](https://linear.app/settings/account/security) |

Tokens are kept in the system keychain. Click **Load columns** to read the board's columns (Jira statuses,
Trello lists, Linear workflow states), then choose:

- **Take tickets from**: the column the agent works on, e.g. *To Do*.
- **While the agent works, move to**: e.g. *In Progress*, or leave the ticket where it is.
- **When fixed, move to**: e.g. *Testing* or *In Review*.
- **When it needs a person, move to**: e.g. *Blocked*, or back where it was.
- **Assigned to**: **Anyone**, **Me** (whoever the token belongs to) or **Me, or nobody yet**. On a shared board,
  **Me** keeps the agent on your own tickets.
- **Only tickets labelled**: only pick up tickets with this label (e.g. `agent`). Leave it empty for every ticket in the column.

In Jira a ticket moves through your workflow's transitions: if there's no transition from where the ticket is
to the column you chose, the move is skipped and the reason shows in the history.

## The agent

- **Project**: the DevCat project the board works for. The agent gets that project's shared memory (what
  your agents learned there, the same memory your chats use) and its connections, like a chat in the
  project. Leave it on **From the repository** to use the project whose code folder holds the repository
  (shown next to the option), or the repository's own memory when no project holds it. Picking a project
  with code folders fills in the **Repository** when it's empty. In focus mode the Tickets list shows the
  focused project's boards (and those in no project). The agent can change data on a connection only when
  its **AI / MCP access** is **Full**, as for agents over MCP: it works on text from the tracker, unattended.
- **Repository**: the git repository the agent works in, or a folder of repositories (say, a front end and its
  API next to each other). A single repository must have no uncommitted changes when a ticket starts, so the
  agent's work never mixes with yours. In a folder, each repository gets the ticket's branch and the agent
  changes the ones the ticket needs; a repository with uncommitted changes (or not on a branch) is left alone,
  and the ticket's history says so.
- **AI connection**: Claude Code (your login, or a Claude Code connection with an API key) or a DevCat Agent
  connection (any model API). Optionally a **Model** (`sonnet`, `haiku`…).
- **Instructions for every ticket**: conventions, where things are, how to test. They're added to each ticket.
- **Let the agent run commands**: off by default, so the agent can only read and edit files in the
  repository. Turn it on to let it build and run tests. It will then run any command it decides to, as you.

## Fix tickets

Pick tickets in the list and click **Fix with the agent**, or use the robot icon on a single ticket. Tickets
are done one at a time; the panel on the right shows what the agent is doing, and **Stop** stops it.

For each ticket:

1. DevCat makes the branch `devcat/<key>-<title>` from the branch the repository is on (in a folder: in each
   repository), and moves the ticket to your *working* column.
2. The agent reads the ticket (title, description, labels, recent comments) and makes the change. The
   ticket's images (screenshots attached in Jira or Trello, or pasted into a Linear description) are shown to
   it: png, jpeg, gif or webp, up to 6 of them and 5 MB each. Other attachments are only named.
3. It answers **fixed**, **needs a person** (with its questions) or **couldn't fix**.
4. DevCat commits what changed on the branch (unfinished work too, marked `WIP`) and switches the
   repository back to the branch it was on. In a folder, the repositories the agent didn't change have their
   branch removed, and the comment names the ones it did. With **Push** on, a fixed ticket's branch is pushed to `origin`.
5. The ticket gets a comment (the summary, how to test, the branch) and moves to *done* or *needs a person*.

If the agent is stopped or crashes, the ticket goes back where it was, without a comment. A ticket the agent
changed nothing for has its branch removed.

## Watch a column

Turn on **Watch** (in the board's toolbar or its settings): every few minutes DevCat checks the column and
works on new tickets, a few at a time. A ticket the agent has already tried isn't picked up again by itself; fix
it again from the list when it's ready. Watching only runs while DevCat is open, and skips a round while the
repository has uncommitted changes.

## Slack

Paste a Slack [incoming webhook](https://api.slack.com/messaging/webhooks) in the board's settings and click
**Test**. Each finished ticket is posted to that channel: what happened, where the ticket went, its link and
the summary. Mattermost and Google Chat webhooks work too.

## Good to know

- The ticket's text goes to your AI connection's provider, like anything else you give the agent.
- The agent never talks to the tracker: DevCat does the moves and comments. The agent is told to treat the
  ticket as a task description, not as instructions, but a ticket can still ask for something harmful. Keep
  **Let the agent run commands** off for boards where anyone can write tickets.
- Tickets are desktop only: DevCat Mobile and the browser version don't show them.
