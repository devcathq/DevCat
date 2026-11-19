# Tickets

Connect a Jira project, Trello board, Linear team or GitHub repository and let your coding agent work through its tickets. You
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
| GitHub | Repository (`owner/name`); **API address** `https://api.github.com` (GitHub Enterprise Server: `https://<host>/api/v3`) | [Personal access token](https://github.com/settings/personal-access-tokens/new): fine-grained with Issues, Pull requests and Contents read and write, or classic with the `repo` scope |

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
- **Working copy**: **In place** (the agent works in the repository itself, as above) or **A worktree for each
  ticket** (below). Boards made before this setting stay in place.
- **AI connection**: Claude Code (your login, or a Claude Code connection with an API key) or a DevCat Agent
  connection (any model API). Optionally a **Model** (`sonnet`, `haiku`…).
- **Default prompt**: what the agent is told with every ticket: conventions, where things are, how to test. A
  ticket you fix with a prompt of its own (below) gets that prompt instead.
- **Let the agent run commands**: off by default, so the agent can only read and edit files in the
  repository. Turn it on to let it build and run tests. It will then run any command it decides to, as you.

## Fix tickets

Pick tickets in the list and click **Fix with the agent**, or use the robot icon on a single ticket. Tickets
are done one at a time; the panel on the right shows what the agent is doing, and **Stop** stops it.

To tell the agent something for particular tickets, click **With a prompt…** (or the speech bubble on a single
ticket). The prompt starts as the board's default prompt; what you write replaces it for those tickets, that
time only. The ticket's text, comments and images still go to the agent, and DevCat still moves and comments
on the ticket as the board says. The history shows the prompt a ticket was fixed with. Tickets the board picks
up by itself (**Watch**) always get the default prompt.

For each ticket:

1. DevCat makes the branch `devcat/<key>-<title>` from the branch the repository is on (in a folder: in each
   repository), and moves the ticket to your *working* column. The prefix (`devcat/`) is set in the board's
   settings under **Branch**. There you can also choose **An existing branch** (say `main` or `staging`): DevCat
   checks it out instead (with **Push** on, it's pulled from `origin` first), and fixed tickets are committed straight
   onto it. Unfinished work still goes on a new `devcat/…` branch, so it never lands on yours. In a folder,
   repositories without that branch are left alone.
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

### A worktree for each ticket

With **Working copy** set to **A worktree for each ticket**, the agent never touches your checkout: keep your
uncommitted work and any branch in the repository while it works. The repository must be one git repository
(not a folder of them). For each ticket:

1. DevCat fetches from `origin` and makes a git worktree at `.worktrees/<key>-<title>` inside the repository. Its
   branch is the ticket's own branch when it has one already (an earlier run's work, here or on `origin`), else a
   new `devcat/<key>-<title>` from the **Base branch** on `origin` (empty: `origin`'s default branch).
2. DevCat adds `.worktrees/` to the repository's local exclude file (`.git/info/exclude`), so the folder never
   shows as untracked and nothing needs committing.
3. DevCat copies the **Files to copy** from the repository into the worktree, for example `.env` (one path a line;
   a folder copies with everything in it). A worktree has only the committed files otherwise. DevCat copies only
   files that git ignores, because the agent's commit would take any other file to `origin`; the ticket's history
   tells you about a file it didn't copy.
4. DevCat runs the **Setup command** in the worktree (for example `pnpm install` or `go mod download`). It runs
   under the same rules as the agent's commands: only on a board that lets the agent run commands, as you, within
   the run's time limit, and **Stop** ends it. Its output shows in the board's activity.
5. The agent works in the worktree. DevCat commits there and, with **Push** on, pushes a fixed ticket's branch.
6. DevCat removes the worktree. The branch keeps the commits (a run that changed nothing leaves no branch).

If the setup command fails, the agent doesn't start: the run ends as failed, and the end of the command's output
is in the history and the ticket's comment. DevCat removes the worktree, and nothing the command changed is
committed.

The guard against changes to git's settings works the same: if the agent changed the repository's git config,
attributes or hooks, DevCat runs no git, and the worktree stays as it is for you to look at.

If DevCat stops in the middle of a run (a crash), the next start removes what that run left in `.worktrees`
(the branch stays). A worktree a run left for you to look at stays until you remove it.

A ticket's branch can be checked out in one place only. If it's checked out in another worktree, or in the
repository itself, the run fails and names that folder: switch it to another branch (or remove the worktree), then
run the ticket again.

### GitHub boards

On a GitHub board the repository's labels are the columns. A new board takes issues labelled
`ready-for-agent`, labels them `agent-working` while the agent works, `agent-review` when fixed and
`ready-for-human` when it needs a person; change any of them in the board's settings. Saving the board checks the
token and the repository, reads the token owner's login, and creates the labels the repository doesn't have yet.

- The agent takes open issues with the trigger label (never pull requests), filtered by **Assigned to**.
- Moving an issue removes its current state label and adds the new one, so it always has exactly one.
- Branches are named `<your login>/<number>-<title>`, e.g. `octo/12-fix-the-login`.
- New GitHub boards start with **A worktree for each ticket**, and the branch is always pushed: a fixed issue's
  branch gets a draft pull request into the default branch (or the **Base branch**) that says `Closes #<number>`.
  The issue's comment has the summary, how to test and the pull request's link, and the history links it too.
- Labels, comments and pull requests appear as the token's owner.

When the board watches the repository, an issue with the trigger label waits until it passes three checks:

- **Who added the label.** The person who last added it must be able to write to the repository (write,
  maintain or admin), or be listed in **Also take labels from** (e.g. a triage bot's login). A label from anyone
  else is ignored, so a stranger can't make the agent run commands on your computer. This needs a token that can
  write to the repository.
- **Blockers.** The issue must have no open blocker: none in GitHub's issue dependencies ("Blocked by") and none
  in a `Blocked by: #12, #13` line in its body. It starts at the first check after its last blocker closes.
- **The backlog.** The label must have been added after the board was created, so a new board doesn't start on
  every issue that was labelled before it. To send one of those to the agent, fix it from the list.

Fixing issues from the list skips these checks: you choose them yourself.

Each entry in the history has **open chat**: it opens the agent's conversation for that ticket in the AI
workspace, where you can read what it did and carry on from there.

## Watch a column

Turn on **Watch** (in the board's toolbar or its settings): every few minutes DevCat checks the column and
works on new tickets, a few at a time. A ticket the agent has already tried isn't picked up again by itself; fix
it again from the list when it's ready. Watching only runs while DevCat is open, and (in place) skips a round
while the repository has uncommitted changes.

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
