# Firefox in a cell, driven over the cell's stream

This recipe puts Firefox in a desktop cell and drives it with Mozilla's MCP server, [`@mozilla/firefox-devtools-mcp`](https://www.npmjs.com/package/@mozilla/firefox-devtools-mcp), run inside the cell. Nothing listens on this machine: Firefox's automation ports are on the guest's loopback, and the MCP server talks over the stdin and stdout of `adytum exec`. [`firefox.sh`](firefox.sh) runs every step below, and the live test `crates/adytum-cli/tests/desktop.rs` runs it.

```text
CELL=adytum-ff sh docs/recipes/firefox.sh
```

## The steps

1. **A desktop cell.** `adytum create adytum-ff --desktop` gives the cell's user a display at `DISPLAY=:99`. `adytum view adytum-ff` shows it on this machine; see the [README](../../README.md).
2. **Firefox from Mozilla's APT repository.** The script fetches Mozilla's signing key, checks that its fingerprint is `35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3` as [Mozilla publishes it](https://support.mozilla.org/kb/install-firefox-linux), and stops if it is not. It then adds the repository with that key as its only signer, prefers its packages over Ubuntu's, and installs `firefox`.
3. **Node.js** 24.22.3 from nodejs.org, checked against the sha256 nodejs.org publishes for it. The MCP server needs Node 20.19 or later, and Ubuntu 24.04 has 18.
4. **The MCP server**, `@mozilla/firefox-devtools-mcp` 0.11.0, installed with npm, which checks each package against the registry's integrity hash.
5. **Firefox**, started as the cell's user on the desktop with `--marionette` and `--remote-debugging-port 9222`. Both listen on the guest's loopback only, and Adytum forwards no guest port. A small `user.js` turns off first-run screens. Nothing hides automation from a site: no preference that changes what a page can detect, and no edited user agent.

## Driving it

The MCP server runs in the cell and attaches to that Firefox. An MCP client on this machine starts it as a stdio server:

```text
adytum exec adytum-ff -- firefox-devtools-mcp --connect-existing --marionette-port 2828 --tools pages snapshot input screenshot
```

`--tools` names the tool modules the client gets: pages, snapshots, input and screenshots. The `script` module, which runs JavaScript in the page, is left out. Add modules only when the work needs them.

For a client's configuration, the command is `adytum` and the arguments are the rest of that line. `adytum exec` forwards the client's stdin to the server and its stdout back, and exits with the server's exit code.

## Seeing it

`adytum view adytum-ff` prints a `vnc://127.0.0.1:<port>` address and a password for that view. Open it with any VNC viewer, such as the Mac's Screen Sharing. The screen share starts for the view and stops when it is closed, and its password goes with it.
