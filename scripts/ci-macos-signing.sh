#!/bin/bash
# CI: macOS release signing with DevCat's self-signed certificate (.github/devcat-codesign.crt;
# the .p12 is the APPLE_CERTIFICATE secret, base64).
#
#   ci-macos-signing.sh setup     # needs CERT_P12, CERT_PASSWORD; prints the identity
#   ci-macos-signing.sh cleanup   # run with `if: always()`
#
# The signing keychain lives in $RUNNER_TEMP, and cleanup takes it off the search list again (so
# the script also suits a runner that keeps its state between jobs).
set -euo pipefail

kc="$RUNNER_TEMP/devcat-signing.keychain-db"
crt=.github/devcat-codesign.crt

# The user keychain search list without any DevCat signing keychain (also stale ones from a
# cancelled job). One path per line, quotes and indentation stripped.
others() { security list-keychains -d user | sed -e 's/^[[:space:]]*"//' -e 's/"$//' | grep -v 'devcat-signing' || true; }

case "${1:-}" in
  setup)
    # codesign signs with an untrusted certificate too, but then the designated requirement names
    # it as the leaf instead of the root ("certificate root = H…", as in every release so far),
    # and the release check fails. GitHub's runners have passwordless sudo; on another Mac, trust it
    # once in Keychain Access (System → DevCat Self-Signed → Trust → Code Signing: Always Trust).
    if ! security verify-cert -q -L -c "$crt" -p codeSign > /dev/null 2>&1 \
      && ! security dump-trust-settings -d 2> /dev/null | grep -q 'DevCat Self-Signed'; then
      sudo -n security add-trusted-cert -d -r trustRoot -p codeSign -k /Library/Keychains/System.keychain "$crt" \
        || { echo "::error::$crt isn't trusted for code signing on this runner; trust it once for code signing (Keychain Access → System → DevCat Self-Signed)"; exit 1; }
    fi
    # Our own keychain: Tauri's APPLE_CERTIFICATE import only accepts Apple-issued names
    # ("Developer ID Application: …"). With just APPLE_SIGNING_IDENTITY set, Tauri runs
    # `codesign -s "DevCat Self-Signed"`, which finds it on the search list.
    rm -f "$kc"
    kp=$(openssl rand -hex 16)
    security create-keychain -p "$kp" "$kc"
    security set-keychain-settings "$kc"
    security unlock-keychain -p "$kp" "$kc"
    p12="$RUNNER_TEMP/devcat.p12"
    trap 'rm -f "$p12"' EXIT
    printf '%s' "$CERT_P12" | base64 --decode > "$p12"
    security import "$p12" -k "$kc" -P "$CERT_PASSWORD" -T /usr/bin/codesign > /dev/null
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kp" "$kc" > /dev/null
    list=()
    while IFS= read -r k; do [ -n "$k" ] && list+=("$k"); done < <(others)
    security list-keychains -d user -s "$kc" "${list[@]}"
    security find-identity -v -p codesigning "$kc"
    ;;
  cleanup)
    list=()
    while IFS= read -r k; do [ -n "$k" ] && list+=("$k"); done < <(others)
    [ ${#list[@]} -gt 0 ] && security list-keychains -d user -s "${list[@]}"
    [ -f "$kc" ] && security delete-keychain "$kc"
    exit 0
    ;;
  *) echo "usage: $0 setup|cleanup" >&2; exit 2 ;;
esac
