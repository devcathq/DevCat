# Troubleshooting

Fixes for the problems people run into most. If yours isn't here, open an issue.

## macOS says DevCat can't be opened or is damaged

DevCat is signed but not notarized by Apple. Open *System Settings → Privacy & Security* and click **Open
Anyway**, or run:

```sh
xattr -dr com.apple.quarantine /Applications/DevCat.app
```

For builds older than v0.2.1 that say "is damaged", also run
`codesign --force --deep --sign - /Applications/DevCat.app`.

## Claude Code (or another agent) is "not installed"

DevCat runs the agent CLI you installed. Install it with the command DevCat shows, run it once in a terminal to log
in, then click **Check again**. If you installed it with a version manager (nvm, asdf…), make sure it works in a new
terminal window.

## Fetch, pull or push fails

DevCat never asks for Git passwords. Set up an SSH key with `ssh-agent`, or a Git credential helper, so `git push`
works in a terminal without a prompt. Then try again in DevCat.

## A VPN won't connect

- **Windows:** let DevCat install OpenVPN or WireGuard when it offers, and accept the UAC prompt.
- **macOS / Linux, WireGuard:** install `wireguard-tools`.
- Open **Show logs** on the profile: the VPN's own log usually says what's wrong (wrong credentials, a blocked port).
- Passwordless VPN refuses configs with scripts or plugins; those still connect with the password prompt.

## A database connection times out

- If the database is in a private network, use **Connect through an SSH tunnel**, or set **Network** to the VPN
  you need.
- Click **Test connection** in the form to see the exact error.

## The phone can't reach the computer

- Make sure **Remote access** is on (or `devcat serve` is running) and the computer is awake. **Keep awake** helps.
- On another network the phone needs the relay: check that it isn't turned off (`--relay off`).
- A pairing code is valid for 5 minutes. Make a new one (**New code**, or `pair` in `devcat serve`).
- Don't run `devcat serve` and the app's Remote access at the same time.

## Still stuck?

[Open an issue](https://github.com/devcathq/DevCat/issues/new) with your system, DevCat version (in the status bar)
and what you see.
