#!/bin/sh
# Firefox in a desktop cell, driven by an MCP server that runs in the cell.
# Usage: CELL=adytum-ff sh firefox.sh
# docs/recipes/firefox.md says what each step is for.
set -eu
: "${CELL:?set CELL to a cell name, such as adytum-ff}"
x() { adytum exec "$@" </dev/null; }

# A Lima cell with a desktop.
adytum create "$CELL" --desktop --cpus 2 --memory 4096 >/dev/null
adytum start "$CELL" >/dev/null

# Firefox from Mozilla's APT repository, its signing key checked by fingerprint.
x --root "$CELL" -- env DEBIAN_FRONTEND=noninteractive apt-get update -q >/dev/null
x --root "$CELL" -- env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends ca-certificates curl gnupg xz-utils python3 >/dev/null
x --root "$CELL" -- install -d -m 0755 /etc/apt/keyrings
x --root "$CELL" -- curl -fsSL --proto =https -o /etc/apt/keyrings/packages.mozilla.org.asc https://packages.mozilla.org/apt/repo-signing-key.gpg
fpr=$(x "$CELL" -- gpg --show-keys --with-colons /etc/apt/keyrings/packages.mozilla.org.asc | awk -F: '$1 == "fpr" { print $10 }')
[ "$fpr" = 35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3 ] || { echo "firefox.sh: Mozilla's key has the fingerprint '$fpr'" >&2; exit 1; }
printf 'Types: deb\nURIs: https://packages.mozilla.org/apt\nSuites: mozilla\nComponents: main\nSigned-By: /etc/apt/keyrings/packages.mozilla.org.asc\n' |
  adytum exec --root "$CELL" -- tee /etc/apt/sources.list.d/mozilla.sources >/dev/null
printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' |
  adytum exec --root "$CELL" -- tee /etc/apt/preferences.d/mozilla >/dev/null
x --root "$CELL" -- env DEBIAN_FRONTEND=noninteractive apt-get update -q >/dev/null
x --root "$CELL" -- env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends firefox >/dev/null

# Node.js from nodejs.org, checked against the sha256 nodejs.org publishes.
x --root "$CELL" -- curl -fsSL --proto =https -o /tmp/node.tar.xz https://nodejs.org/dist/v24.22.3/node-v24.22.3-linux-arm64.tar.xz
echo "6e51a2ada00f0011cc266f65d98922b403d7ad88f809ca2aa615a5fe1ce6e86c  /tmp/node.tar.xz" | adytum exec --root "$CELL" -- sha256sum -c --quiet -
x --root "$CELL" -- tar -x -J -f /tmp/node.tar.xz -C /usr/local --strip-components 1 --no-same-owner
x --root "$CELL" -- rm /tmp/node.tar.xz

# The MCP server, at a pinned version.
x --root "$CELL" -- npm install -g --no-fund --no-audit --loglevel=error @mozilla/firefox-devtools-mcp@0.11.0 >/dev/null

# A profile of the cell's user's own, with a few first-run screens off.
x "$CELL" -- mkdir -p /home/cell/firefox
printf '%s\n' \
  'user_pref("browser.shell.checkDefaultBrowser", false);' \
  'user_pref("browser.aboutwelcome.enabled", false);' \
  'user_pref("datareporting.policy.dataSubmissionEnabled", false);' |
  adytum exec "$CELL" -- tee /home/cell/firefox/user.js >/dev/null

# Firefox on the desktop's display, with Marionette (127.0.0.1:2828) and the
# remote debugging port (127.0.0.1:9222) on the guest's loopback.
x --root "$CELL" -- systemd-run --unit adytum-firefox --uid cell --gid cell \
  --setenv DISPLAY=:99 --setenv HOME=/home/cell \
  firefox --no-remote --profile /home/cell/firefox --marionette --remote-debugging-port 9222 >/dev/null
for _ in $(seq 1 60); do
  x "$CELL" -- python3 -c "import socket; socket.create_connection(('127.0.0.1', 2828), 1)" 2>/dev/null && break
  sleep 1
done
echo "firefox.sh: $CELL is ready"
