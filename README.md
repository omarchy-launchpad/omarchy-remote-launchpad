# Seams

For the approved replacement of xpra with our own Rust Linux agent and Mac
renderer, read [the standalone engine handoff](HANDOFF.md). The implementation
described below is the current xpra-backed version.

A macOS menu bar application for saving SSH connections and running remote Linux X11
applications in separate Mac windows. Written in Rust with native AppKit
controls. No browser, webview, Swift, or async runtime.

Xpra handles window rendering, input, resizing and clipboard forwarding. Seams
handles connection profiles, launching, reconnecting and session controls. The
Xpra client is currently a separate required installation, not embedded in the
app. Remote windows belong to the Xpra process and share its Dock identity.

Seams lives in the macOS menu bar without a Dock icon. Its menu lists saved
connections with Launch, Reconnect, Disconnect and Stop session actions. Choose
**Manage connections…** to open configuration and diagnostics. Closing that
window leaves Seams and its clients running. Use **Quit Seams** in the menu to
disconnect local clients and quit. The manager opens automatically when there
are no saved connections or the saved configuration needs repair; otherwise
Seams starts quietly in the menu bar.

Packaged and development builds share a per-user instance lock. Opening a
second copy shows the running copy's manager and exits without adding another
menu bar item. Quit the running packaged copy before starting `make dev` if you
want to switch to a development build. Stale lock files are safe to leave in
place; the operating system releases the lock when its owning process exits.

## Build and open

Requires macOS 13+, current stable Rust and Apple command line tools. Builds for
the current Mac architecture.

```sh
make run
```

This produces `dist/Seams.app`, signed ad hoc for local use. `make dmg` creates
`dist/AppSeams.dmg`. Public distribution needs a Developer ID signature and
notarization. No developer credentials are required for local builds.

Three direct dependencies, all Apple bindings: `objc2`, `objc2-foundation`,
`objc2-app-kit`. Core connection, process and storage code uses Rust's standard
library. Versions are pinned in `Cargo.lock`.

## Configure once

1. Install the [macOS Xpra client](https://github.com/Xpra-org/xpra/wiki/Download).
   Seams detects common application and Homebrew paths. If needed, use
   **Choose executable** and select `Xpra.app/Contents/MacOS/Xpra` or your
   installed `xpra` command.
2. Install Xpra and an X11 server backend such as Xvfb on Linux using your
   distribution's packages. Install `xterm` for an initial test.
3. Establish SSH trust and key authentication once with `ssh your-host` in
   Terminal. Seams uses OpenSSH, existing `~/.ssh/config`, known hosts and SSH
   agent. It never saves passwords or disables host key verification. Unlock
   encrypted keys in your SSH agent before connecting.
4. Add a connection in Seams. Enter a host or SSH alias, optional user/port/key,
   and remote application command. Leave user and port empty to inherit SSH
   config. Each entry uses its own display number, starting at 100.
5. Choose **Check SSH host**, then **Launch app**. Status and diagnostics show
   errors, including missing remote packages or SSH trust/key problems.

For example, save host `omarchy` and command `firefox --no-remote`. Xpra starts
an independent X11 session over SSH, and its client presents each remote
window alongside local applications. No Linux desktop is streamed. Applications
must support X11. GTK and Qt receive X11 backend environment variables.

Commands are deliberately executed on the remote account and may include
arguments. Only enter commands you intend to run. SSH identity paths must be
absolute paths on this Mac; `~` is not expanded in that field.

## Session controls

- **Launch app** creates the saved display and starts its command. If that
  session already exists, use Reconnect instead of launching it again.
- **Reconnect** attaches to the same display without starting another app.
- **Disconnect** closes the local client and SSH transport. Remote apps keep
  running. Quitting Seams also disconnects its local clients.
- **Stop session** terminates the remote session and its apps after a
  confirmation. Disconnect first if its client is still running.
- **Delete connection** removes only saved configuration. It does not stop
  remote apps.

Save edits before switching entries. Disconnect an entry's active local client
before changing its configuration. Each saved entry has a distinct display
number to make reconnect unambiguous. Reserve these numbers for Seams on the
remote host; existing X11/Xpra sessions may already occupy them.

Profiles live in `~/Library/Application Support/Seams/connections.tsv`, with
private directory/file permissions. The versioned UTF-8 format escapes tabs,
newlines and backslashes. Writes use a temporary file plus atomic rename.
Malformed files are left intact and saving is blocked until repaired or backed
up. Diagnostic output stays in memory and is bounded.

## Development and debugging

```sh
make dev
```

Runs a debug build with backtraces and watches `src/`, `packaging/`, Cargo
manifests and the Makefile. Saving a changed file rebuilds and relaunches the
app. Compile errors leave the previous app running. Ctrl-C stops the watcher
and its dev app. No watcher package is required.

This is rebuild/relaunch, not in-process hot swapping. Save connection edits
before reloading. Local Xpra clients disconnect on restart, while remote apps
continue running. Select Reconnect after relaunch. Close modal dialogs if they
prevent the app from quitting during a reload. Packaging edits are watched but
need `make build` to update the application bundle.

For a single debug run with terminal output:

```sh
RUST_BACKTRACE=1 cargo run --bin seams
```

For breakpoints and crash inspection:

```sh
make debug
```

At the LLDB prompt:

```text
breakpoint set --file model.rs --line 147
run
bt
continue
```

The app's Diagnostics panel contains Xpra and SSH output. Build errors and
Rust panic backtraces appear in the launching terminal. Saved settings are
shared between debug and packaged builds. Run one copy while debugging to
avoid competing writes to the connections file.

## Verification and current limits

```sh
make test
make check
```

Core tests cover URI construction, SSH argument isolation, key paths with
quotes/spaces, reconnect behavior and persistence. These checks cannot prove
remote video/input behavior. A reachable Linux host with Xpra is required for
that demo. No LAN latency or frame-rate claim has been measured yet.

Xpra supplies the remote windows and its own Mac integration. Independent Dock
entries per remote application, native menu export, automatic Linux package
installation, automatic network reconnect, and a bundled rendering engine are
not implemented. Clipboard compatibility depends on the installed Xpra builds.
Audio is disabled initially. This version launches apps in new managed X11
sessions; it does not import windows from an existing Linux desktop.

Implementation follows Xpra's documented [seamless mode](https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Seamless.md)
and [SSH transport](https://github.com/Xpra-org/xpra/blob/master/docs/Network/SSH.md).
