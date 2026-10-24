# devcat serve (CLI)

Share a computer with DevCat Mobile and the browser version from a terminal, without opening the app.

Use it on a server or dev box, over SSH, or simply with the window closed. Phones pair and work exactly as with the
app: agents and terminals, files, databases, VPNs and Kubernetes. It uses the app's own data folder, connections
and paired phones.

## Start it

Install the CLI ([Install and update](install.md#install-only-the-cli)), or use the one inside the app:

```sh
devcat serve                                            # the CLI, or Linux .deb / .rpm installs
/Applications/DevCat.app/Contents/MacOS/devcat serve    # macOS app
```

It prints the computer's addresses and a pairing QR code in the terminal. Scan it with the phone, then answer the
pairing request in the terminal: `f` (full access), `r` (read-only) or `n` (no).

The CLI on its own has no window code, so it runs on a bare server. On Linux it only needs `libdbus`.

## Commands while it runs

| Type | To |
| --- | --- |
| `pair` (or just <kbd>Enter</kbd>) | Show a new pairing QR code |
| `status` | Addresses, relay and connected phones |
| `devices` | List paired phones, numbered |
| `access <n> full` / `access <n> readonly` | Change a phone's access |
| `revoke <n>` | Unpair a phone |
| `log` | Recent activity from phones |
| `sync` | Computers this one shares its agents' [memory](memory.md#other-computers) with |
| `sync join <code>` | Share memory with a computer: paste the code from its **Memory → Other computers**, then allow it there |
| `sync now` / `sync remove <n>` / `sync log` | Sync memory now, stop sharing with one, recent memory sync activity |
| `quit` | Stop (<kbd>Ctrl+C</kbd> works too) |

While it runs, `devcat serve` keeps the memory in sync with the computers it was paired with.

## Options

| Option | Meaning |
| --- | --- |
| `--port <n>` | Port phones connect to (default 47100; UDP, and TCP for browsers) |
| `--name <name>` | Name phones show for this computer |
| `--relay public\|off\|<url>` | How phones on other networks get here |
| `--public-addr <host>` | A public address phones outside this network can use |
| `--no-browser` / `--browser` | Turn the browser version off or on again |
| `--approve ask\|full\|readonly` | Who answers pairing requests (this run only) |
| `--replace` | Another DevCat is using the port: stop it without asking (this run only) |
| `--data-dir <dir>` | DevCat's data folder (this run only) |

`--port`, `--name`, `--relay`, `--public-addr` and `--no-browser` are saved and also used by the app. Run
`devcat help` for the full list.

## Run it in the background

With no terminal to answer in (systemd, `nohup`, the app's Windows binary), start it with `--approve full` or
`--approve readonly`. Any phone with the current pairing code is then allowed, and new codes are printed as links.

> **Warning:** Don't run `devcat serve` while the app has **Remote access** on. They would share one port and
> identity.

## Stop one you can't reach

A `devcat serve` left running in the background, in a closed terminal or from before an update keeps the port,
and there's nowhere to type `quit`. Run:

```sh
devcat stop
```

It finds the DevCat using the port (`--port <n>` for another one) and stops it the same way `quit` does. If it
doesn't stop within 15 seconds, `devcat stop --force` kills it.

Starting `devcat serve` while another DevCat has the port also says which one and offers to stop it and take
over. Without a terminal to answer in, pass `--replace` to do that without asking.
