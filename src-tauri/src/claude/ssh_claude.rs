//! Claude Code in an SSH session's folder when the server doesn't have it: rather than run
//! `claude` there (exit 127: "claude: not found"), DevCat starts the Claude Code of this computer in
//! a scratch folder of its own, with `devcat mcp` (`mcp_sync::ssh_config`) and a system prompt that
//! sends its work to the server through the `ssh_*` tools (`agent_tools/ssh.rs`). Nothing is
//! installed on the server.
//!
//! Only for a saved session whose AI access the user turned on (those tools are opt-in, see
//! AGENTS.md), and only from the window (`pty::open` with origin "desktop"): a phone never gets it.
use super::remote::{self, sh_quote, Target};
use crate::error::{AppError, AppResult};
use crate::models::McpAccess;
use std::path::PathBuf;
use std::time::Duration;

/// How Claude Code starts here instead: in `cwd`, with `args` added.
pub struct Local {
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

/// Whether the host finds `claude` the way a terminal there does (a login shell). Unknown (it
/// couldn't be asked) counts as yes: the terminal then shows what ssh says, as before.
async fn host_has_claude(t: &Target) -> bool {
    let script = format!("exec \"${{SHELL:-/bin/sh}}\" -lc {}", sh_quote("command -v claude >/dev/null 2>&1"));
    match tokio::time::timeout(Duration::from_secs(30), remote::exec(t, &script, &[], None, Duration::from_secs(25))).await {
        Ok(Ok(out)) => out.code != 1 && out.code != 127,
        _ => true,
    }
}

/// For a Claude Code tab in `t` (a saved session's folder, on a Linux or macOS host): None when
/// it runs on the host as before; else how to run it here.
pub async fn local_for(t: &Target) -> AppResult<Option<Local>> {
    let Some(s) = t.session.as_deref() else { return Ok(None) };
    if remote::host_os(t).await == remote::HostOs::Windows || host_has_claude(t).await {
        return Ok(None);
    }
    let st = crate::state::St::get();
    let c = st.store.get(&s.id).map_err(|_| AppError::msg("This SSH session was deleted."))?;
    let access = crate::agent_tools::ssh_access(&c);
    if access == McpAccess::Off {
        return Err(AppError::msg(format!(
            "Claude Code isn't installed on {name}. To have the Claude Code on this computer work there instead (nothing is installed on the server), \
             set {name}'s AI / MCP access to Read or Full (Edit connection → Advanced), then open this tab again.",
            name = c.name
        )));
    }
    let cwd = crate::mcp_sync::data_folder("ssh-work")?.join(&s.id);
    std::fs::create_dir_all(&cwd)?;
    let config = crate::mcp_sync::ssh_config(c.project_id.as_deref(), &cwd)?;
    let file = cwd.join("devcat-mcp.json");
    std::fs::write(&file, serde_json::to_string_pretty(&config).unwrap_or_default())?;
    let who = if s.user.as_deref().unwrap_or("").is_empty() { s.host.clone() } else { format!("{}@{}", s.user.as_deref().unwrap_or(""), s.host) };
    let tools = if access == McpAccess::Full {
        "`ssh_read`, `ssh_glob` and `ssh_grep` to read and search, `ssh_run` to run commands (git, builds, tests), `ssh_edit` and `ssh_write` to change files"
    } else {
        "`ssh_read`, `ssh_glob` and `ssh_grep` to read and search (the user allowed reading only: you can't run commands or change files there)"
    };
    let prompt = format!(
        "You are working on a server, not on this computer. The user's project is on \"{name}\" ({who}), reached over SSH, in the folder {path} there. \
         Claude Code isn't installed on that server, so you run on the user's computer and work on the server through DevCat's MCP tools, always with \
         `session` \"{id}\": {tools}. Paths are paths on the server: write them absolute or as `~/…` (relative ones start in the session's start folder). \
         Don't use your own Bash, Read, Edit, Write, Glob or Grep tools for the project: they act on this computer, whose working folder is only an empty \
         scratch folder. Don't run `ssh` yourself or ask for the password: DevCat logs in as the session does.",
        name = c.name,
        id = c.id,
        path = t.path,
    );
    Ok(Some(Local { cwd, args: vec!["--mcp-config".into(), file.to_string_lossy().into_owned(), "--append-system-prompt".into(), prompt] }))
}
