# Install and update

Download DevCat for your computer, install the CLI on a server, and keep both up to date.

## Download the app

Get the installer for your system from the [latest release](https://github.com/devcathq/DevCat/releases/latest):

| System | File |
| --- | --- |
| Windows | `DevCat_<version>_x64-setup.exe` (or the `.msi`) |
| macOS, Apple silicon | `DevCat_<version>_aarch64.dmg` |
| macOS, Intel | `DevCat_<version>_x64.dmg` |
| Linux | `.AppImage` (any distribution), `.deb` (Debian, Ubuntu) or `.rpm` (Fedora) |
| Android | `DevCat-Mobile.apk`, see [DevCat Mobile](mobile.md) |

Or install from a terminal:

```sh
brew install --cask devileya/devcat/devcat                                          # macOS (Homebrew)
curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh -s -- --app            # macOS, Linux (AppImage)
& ([scriptblock]::Create((irm https://devcathq.github.io/DevCat/install.ps1))) -App   # Windows (PowerShell)
```

The scripts check the download's SHA-256 and install for your user only, without `sudo`. Add `--version v0.7.0`
to pick a version.

### First start on macOS

DevCat is signed but not notarized by Apple, so macOS asks once:

1. Open DevCat. macOS says it can't check it.
2. Open *System Settings → Privacy & Security* and click **Open Anyway**.

Or run `xattr -dr com.apple.quarantine /Applications/DevCat.app` once.

## Install only the CLI

The CLI runs [`devcat serve`](devcat-serve.md) without a window, for servers and SSH sessions:

```sh
brew install devileya/devcat/devcat-cli                          # macOS, Linux (Homebrew)
curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh     # macOS, Linux → ~/.local/bin
irm https://devcathq.github.io/DevCat/install.ps1 | iex          # Windows (PowerShell)
```

Add `--dir <folder>` to choose where the CLI goes.

## Updates

DevCat checks GitHub for new versions and shows a popup with **Update and restart**, **Later** and **Skip this
version**. Updates are signed and checked before they're installed.

To check yourself, use any of these:

- *DevCat → Check for Updates…* in the macOS menu bar
- *Check for updates…* in the project menu (top left)
- Click the version number in the status bar

> **Note:** Linux `.deb` and `.rpm` installs get a download link instead. The AppImage updates itself.
