#!/bin/sh
# The release notes for a version: its section of CHANGELOG.md (what's new), then how to download
# and install. The GitHub release and latest.json (the app's "new version" dialog) both use them.
#
#   scripts/release-notes.sh 0.8.1 > notes.md
#
# Without a CHANGELOG.md section for the version, "what's new" is GitHub's list of the pull
# requests merged since the previous release (needs `gh` with a token, as in CI).
# The app shows only the part above the `<!-- downloads -->` line.
set -eu

version="${1:?usage: release-notes.sh <version>}"
version="${version#v}"
repo="${GITHUB_REPOSITORY:-devcathq/DevCat}"
dir=$(cd "$(dirname "$0")/.." && pwd)

# "## 0.8.1 - 2026-10-22" up to the next "## " heading
news=$(awk -v v="$version" '
  /^## / { if (on) exit; on = ($2 == v) ; next }
  on { print }
' "$dir/CHANGELOG.md" | sed -e '/./,$!d')

if [ -z "$news" ] && command -v gh > /dev/null 2>&1; then
  news=$(gh api "repos/$repo/releases/generate-notes" -f tag_name="v$version" -q .body 2> /dev/null || true)
fi
[ -n "$news" ] || news="Bug fixes and improvements."

printf '%s\n\n' "$news"
cat << EOF
<!-- downloads -->
---

### Download

Already on v0.3.0 or later? DevCat offers the update itself.

- **macOS (Apple Silicon):** \`DevCat_${version}_aarch64.dmg\`
- **macOS (Intel):** \`DevCat_${version}_x64.dmg\`
- **Windows:** \`DevCat_${version}_x64-setup.exe\` or \`DevCat_${version}_x64_en-US.msi\` (OpenVPN and WireGuard installers included; DevCat offers them the first time you connect)
- **Linux:** \`.AppImage\`, \`.deb\` (Debian/Ubuntu) or \`.rpm\` (Fedora/RHEL)
- **CLI only** (\`devcat serve\`: phones and browsers without the app; servers, SSH): \`devcat-cli-<platform>.tar.gz\` / \`.zip\`, or
  \`curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh\` · \`brew install devcathq/devcat/devcat-cli\`

The builds are not notarized by Apple / signed for Windows yet.
**macOS:** after dragging DevCat to Applications, open it once; when macOS says it can't verify
the developer, go to *System Settings → Privacy & Security* and click **Open Anyway**
(or run \`xattr -dr com.apple.quarantine /Applications/DevCat.app\` in Terminal).
**Windows:** SmartScreen may warn: choose "More info" → "Run anyway".
EOF
