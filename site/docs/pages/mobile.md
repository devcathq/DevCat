# DevCat Mobile

Get notified when your agent finishes or needs you, answer it from your phone, and reach your databases, VPNs and
clusters on the go.

![DevCat Mobile listing the running agents](../img/mobile-agents.png)

## What you can do from the phone

- **Agents**: see which agents are running, open one's live terminal and answer it, start a new chat or resume an
  old session.
- **Files**: browse a project, preview code, Markdown, images, PDFs and spreadsheets, and edit text files.
- **Data**: browse and edit tables, run queries, edit Redis keys.
- **Network**: switch VPNs and tunnels on and off.
- **Cluster**: check pods, logs and deployments.

## Pair your phone

1. On the computer, click the phone icon in the title bar and turn on **Remote access**. A QR code appears.
2. Point the phone's camera at it. It opens DevCat in the phone's browser.
3. Choose **Open in DevCat app** if you have the app, or **Continue in browser** to use it right there with
   nothing to install.
4. Give the phone a name and tap **Connect**.
5. On the computer, click **Allow**, with **full access** or **read-only**.

> **Tip:** The Android app is the `DevCat-Mobile.apk` file on the
> [releases page](https://github.com/devcathq/DevCat/releases/latest). For iPhone, build it with Xcode for now.

## How it connects

There's no DevCat server and no account. The phone talks to DevCat on your computer, encrypted end to end with keys
made when you paired:

- **Same Wi-Fi**: directly.
- **Anywhere else** (mobile data, another network): through a relay that only passes the encrypted traffic on.
  DevCat uses free public relays by default, or your own.

Your connection profiles and passwords stay on the computer. The phone only remembers which computers it paired
with.

> **Warning:** Browser sessions are encrypted with a key from the QR code, but the page itself is loaded over plain
> HTTP. On Wi-Fi you don't trust, use the app, or turn **Allow phone browsers** off.

## Manage phones

In the **Remote access** window on the computer:

- Change a phone between **full access** and **read-only** at any time.
- **Revoke** cuts a phone off immediately.
- **Activity** lists every change a phone made.

## Notifications

"Finished" and "needs you" notifications come through the free [ntfy](https://ntfy.sh) app, so they arrive even
when DevCat Mobile is closed. Turn them on under **More → Notifications** on the phone.

DevCat learns an agent's state from Claude Code hooks it adds to the terminals it starts (macOS and Linux).
[Scheduled DevCat Agent jobs](ai-connections.md#scheduled-jobs) notify you the same way when a run finishes or
needs your OK.

## Without the app window

On a server, over SSH, or with the window closed, run [`devcat serve`](devcat-serve.md) instead. Phones pair and
work the same way.
