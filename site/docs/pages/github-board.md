# Set up a GitHub board

From a GitHub repository to an agent that turns labelled issues into draft pull requests, step by step.

A GitHub board watches a repository's issues. When an issue gets the label `ready-for-agent`, DevCat makes a
worktree and a branch for it, lets your coding agent fix it, pushes the branch and opens a draft pull request. The
issue's labels show where it is. [Tickets](tickets.md) describes every setting; this page is the short way through.

## 1. Before you start

You need:

- **A local clone of the repository**, with `origin` pointing at GitHub. DevCat fetches and pushes with git in this
  clone, with your usual git login, so you need push access to the repository.
- **Claude Code or a DevCat Agent connection** in DevCat (see [AI connections](ai-connections.md)). The board's agent
  runs with it, unattended.
- **A personal access token** for the repository (next step). DevCat uses it for issues, labels, comments and pull
  requests. The agent never gets it.

## 2. Create the token

Use a fine-grained token if you can. **Create a token** in the board dialog opens GitHub's form with the name, the
owner and the permissions filled in (type the repository in the dialog first). On GitHub:

1. **Resource owner**: the repository's owner (your account, or the organization).
2. **Repository access**: **Only select repositories**, then choose the repository. The form can't fill this in.
3. **Repository permissions**: **Issues**, **Pull requests** and **Contents**: **Read and write**. **Metadata**
   (read) comes with them.
4. **Expiration**: when the token runs out, paste a new one in the board's settings.
5. Click **Generate token** and copy it.

A classic token with the `repo` scope works too.

- **Organizations** can block fine-grained tokens, or make them wait for an owner's approval (the token shows as
  pending until then). Ask an owner, or use a classic token. An organization with single sign-on (SAML) needs the
  classic token authorized for it (**Configure SSO** next to the token).
- **GitHub Enterprise Server**: create the token on your server. **Create a token** opens your server's form when
  the board's **API address** is set; fill it in as above.

## 3. Connect the board

Click **Tickets** in the left bar, then **Connect a board**, and choose **GitHub**.

1. **Repository**: `owner/name`, e.g. `acme/shop`. **API address**: leave `https://api.github.com`, or on GitHub
   Enterprise Server `https://<host>/api/v3`.
2. **Personal access token**: paste it. It's kept in the system keychain.
3. **Workflow**: the labels are the columns. A new board starts with these (click **Load columns** to choose other
   labels of the repository):

   | Setting | Default | What it means |
   | --- | --- | --- |
   | **Take issues labelled** | `ready-for-agent` | Ready: the agent may take it |
   | **While the agent works, label** | `agent-working` | A run is going on |
   | **When fixed (draft pull request), label** | `agent-review` | The pull request waits for a person's review |
   | **When the agent has questions, label** | `needs-info` | The issue's comment has the agent's questions |
   | **When it fails (no pull request), label** | `ready-for-human` | A person takes it from here |
   | **When its pull request closes without a merge, label** | `wontfix` | Closed as not planned |

   `ready-for-agent`, `needs-info`, `ready-for-human` and `wontfix` are the labels of Matt Pocock's triage skills;
   `agent-working` and `agent-review` are DevCat's own. An issue always has exactly one of these labels. Saving
   the board creates the labels the repository doesn't have.
4. **Assigned to**: a new board starts with **Me**, the issues assigned to the token's owner, so each person's
   DevCat works on their own issues. **Me, or nobody yet** takes unassigned issues too.
5. **Also take labels from**: logins whose labels count although they can't write to the repository, e.g. a triage
   bot. Labels from anyone else without write access are ignored.
6. **Agent** section:
   - **Repository**: your local clone (**Choose**). **Project**: the DevCat project whose memory and connections
     the agent gets (or leave **From the repository**).
   - **Working copy**: **A worktree for each ticket** (the default for GitHub). Each issue gets its own worktree in
     the clone's `.worktrees` folder, so you can keep working in the clone meanwhile.
   - **Let the agent run commands**: right under **Working copy**. Turn it on to let the agent build and run tests
     (it then runs any command it decides to, as you). A **Setup command** needs it.
   - **Files to copy**: files git ignores that the code needs, one a line, e.g. `.env`. A worktree has only the
     committed files.
   - **Setup command**: runs in each new worktree before the agent starts, e.g. `pnpm install` or
     `go mod download`. If it fails, the run fails with its output.
   - **Base branch**: where new branches start and pull requests go. Empty: the repository's default branch.
     **Branches** shows the names DevCat gives them: `<your login>/<number>-<title>`.
   - **Parallel runs**: how many issues the agent works on at the same time (1 to 4).
   - **AI connection** and **Model**, **Time limit** (the longest one run takes, default 60 minutes) and
     **Default prompt** (conventions, how to test).
   - **Watch the column**: turn it on, and DevCat checks the repository every few minutes (default 10) and takes
     up to 3 issues a check (you can change both).
7. Click **Connect**. DevCat checks the token and the repository, reads the token owner's login and creates the
   missing labels.

## 4. On GitHub

Add `ready-for-agent` to an issue. At the next check, DevCat takes it if it passes three checks:

- **Who added the label**: someone who can write to the repository (write, maintain or admin), or a login in
  **Also take labels from**. A label from anyone else is ignored, so a stranger can't run the agent on your
  computer.
- **Blockers**: no open issue blocks it, in GitHub's issue dependencies ("Blocked by") or in a `Blocked by: #12`
  line in its body. It starts at the first check after its last blocker closes.
- **The backlog**: the label was added after you connected the board. Issues labelled before don't start by
  themselves: fix them from the board's list in DevCat.

Then:

1. The issue gets `agent-working`. The agent reads the issue, the comments of people with write access and the
   images in them.
2. A fixed issue's branch is pushed and gets a draft pull request. It goes into the branch of a `Base: <branch>`
   line in the issue (or in its parent issue), else the **Base branch**, else the default branch. Into the default
   branch it says `Closes #<number>`, into another branch `Part of #<number>`. The agent writes the description in
   the format of the repository's pull request template (`.github/pull_request_template.md`). The issue gets
   `agent-review` and a comment with the summary, how to test and the pull request's link.
3. When the agent has questions, the issue gets `needs-info` and a comment with them. Answer in a comment (the
   agent reads only comments from people with write access), then swap `needs-info` for `ready-for-agent`: the
   next check runs it again, on the same branch, with your answer.
4. When it fails, its work so far is pushed, and the issue gets `ready-for-human` with the reason and the branch.
   To give it back to the agent, add `ready-for-agent` again.
5. With **Watch** on, DevCat follows the pull request. When it merges, the issue loses `agent-review` and is
   closed. When it closes without a merge, the issue is closed as not planned with `wontfix`, and the branch is
   deleted (unless it has newer commits).

## 5. Troubleshooting

- **"GitHub repository owner/name was not found with this token"** when you connect or click **Load columns**: check
  the owner/name. A private repository answers 404 to a token that can't see it: edit the token on GitHub and add
  the repository under **Repository access**, **Only select repositories** (or create a new token, step 2). In an
  organization, the token's **Resource owner** must be the organization, and an owner may have to approve it.
- **"The setup command runs programs … turn on “Let the agent run commands”"**: the switch is in the **Agent**
  section, right under **Working copy**. Turn it on (or click **Turn it on** under the **Setup command**), or
  clear the setup command.
- **An issue stuck in `agent-working` after DevCat crashed or quit during a run**: at the next start, DevCat
  stops a leftover agent from the run that was cut short, moves the issue to the failed label (`ready-for-human`
  by default) with a comment that names the branch, and the run shows in **History** with the outcome "error".
  The comment says to add the trigger label again to try again: the next run continues on the same branch. (On
  other trackers, fix the ticket with the agent from the Tickets tab.) An issue that someone moved after the
  crash is left alone. On macOS and Linux, the agent of an unattended run also stops when DevCat quits or
  crashes.
- **Stop**: **Stop** in the board's **Working** panel ends its runs and empties its queue. A stopped issue goes
  back to the label it had (`ready-for-agent`) without a comment; its work stays committed on the branch in your clone, not pushed.
  With **Watch** on, the next check takes it again, so turn **Watch** off first if it should wait.
- **An issue doesn't start**: check who added the label, its blockers and when the label was added (above), and
  that it's assigned as **Assigned to** says. A label only counts once: to run an issue again, add the label
  again.
