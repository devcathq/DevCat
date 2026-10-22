# DevCat for Homebrew

```sh
brew install devileya/devcat/devcat-cli        # the CLI: devcat serve (macOS, Linux)
brew install --cask devileya/devcat/devcat     # the desktop app (macOS)
```

`devcat serve` shares the computer with [DevCat Mobile](https://github.com/devcathq/DevCat#mobile-app) and phone
browsers without opening the app (servers, SSH sessions). The formula and cask follow DevCat's releases
automatically (`update.sh`, run hourly by `.github/workflows/update.yml`).
