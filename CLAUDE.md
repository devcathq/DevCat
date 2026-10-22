# DevCat: notes for developers and coding agents

## Rule: the SSH workbench is desktop-only

The Xshell-style features (SSH sessions in the sidebar, SFTP tab, quick-command bar, and pasting or
dropping images into a terminal on an SSH host) live in the desktop app only. **Don't add them to
DevCat Mobile** (`mobile/`), the phone dispatcher (`src-tauri/src/remote/dispatch.rs`) or the browser
version, unless you've read the limitations below and the change handles each one explicitly.

Why the phone can't simply reuse the desktop flow:

- **Files land on the wrong machine.** The phone talks to DevCat on the computer, and the computer
  talks to the SSH host. A file the phone sends ends up on the computer (`paste_dir`, agent
  attachments); an agent running on the SSH host can't read that path. Anything meant for the host
  must be copied there (see `remote::stage_files`) and the *host's* path typed into the terminal.
- **The clipboard is the computer's, not the phone's.** `remote_paste_image` reads the desktop OS
  clipboard. From a phone it would paste whatever happens to be on the computer.
- **Saved passwords.** SSH sessions keep their password in the computer's keychain and DevCat types
  it into the login prompt (`pty.rs`, `autopass`). Letting a paired phone open a session would let it
  log in to the server with that password; that needs its own consent and audit design first.
- **Transfers are long-running.** SFTP jobs stream over the computer's `ssh` and report progress as
  desktop window events (`sftp-progress`), which the phone protocol doesn't carry.

What the phone side does today: `list_connections` leaves SSH sessions out, and `pty_open` refuses
an SSH-session folder (`ssh://devcat-…`). Keep it that way unless the work above is done.
