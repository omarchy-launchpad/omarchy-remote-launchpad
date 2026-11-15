# Standalone Rust engine handoff

Date: 2026-11-15. Repository: `app-seams`. Product name: Seams. Download name:
`AppSeams.dmg`.

## Assignment

Replace the current xpra integration with our own Rust remote-window engine.
The user explicitly chose this path. Deliver a macOS menu bar application that
configures an SSH connection, launches an application on Linux, and presents
its X11 windows as individual native Mac windows.

The Mac must work without an xpra, XQuartz, Python, GTK, FFmpeg or other
third-party runtime installation. Ship the Mac application and our Linux
agent. Existing OS frameworks, system SSH and the Linux X server remain
dependencies. Keep third-party Rust crates few and justified. The user asked
for minimal dependencies, not a hand-written replacement for every library.

Implementation language is Rust. Keep AppKit through the existing Objective-C
bindings. The app lives primarily in the menu bar, without a Dock icon. Its
manager is a secondary configuration/diagnostics window. Closing the manager
must leave remote windows and the menu bar running.

This handoff authorizes engineering work on the standalone engine. It does not
claim that engine exists or that remote rendering has been demonstrated.

## Current state to preserve

The repository contains a functioning native configuration application. Its
current remote-window implementation delegates to an external xpra process.
No custom capture agent, frame protocol, native remote-window renderer or
input bridge exists yet.

Relevant ownership:

| File | Current responsibility | Work ahead |
| --- | --- | --- |
| `src/ui.rs` | AppKit manager, status item, saved connection menus, diagnostics | Keep shell; add native remote windows and renderer |
| `src/model.rs` | Profile validation, SSH options, xpra URI/arguments | Keep SSH trust behavior; replace engine-specific configuration |
| `src/session.rs` | Child process groups, bounded logs, xpra lifecycle | Own SSH transport and agent lifecycle instead |
| `src/store.rs` | Versioned profiles, private permissions, atomic saves | Migrate profile semantics safely |
| `src/instance.rs` | Per-user file lock | Preserve single-instance behavior across dev and packaged copies |
| `src/main.rs` | Instance acquisition and app startup | Keep lock alive for app lifetime |
| `scripts/dev-macos` | Debug rebuild/relaunch watcher | Extend watched paths when agent/shared crates are added |
| `scripts/build-macos`, `packaging/Info.plist`, `Makefile` | App bundle, ad-hoc signing, DMG, development commands | Preserve working developer flow |

The three direct dependencies are `objc2`, `objc2-foundation` and
`objc2-app-kit`, restricted to macOS. Cargo feature selection already limits
Apple framework bindings. There is no webview or async runtime.

Settings live under `~/Library/Application Support/Seams/`. Existing profiles
use a versioned escaped TSV file. The `display` field currently identifies an
xpra-managed session, starting at 100. It is **not** the existing Linux desktop
display. The `engine-path` file currently points to xpra. Preserve existing
settings and report migration errors without overwriting them.

The existing instance lock prevents dev and packaged builds from producing two
menu bar entries. A second launch sends a distributed notification to show the
running copy's manager, then exits. File locks release on process exit; the
lock file intentionally remains on disk. Do not remove or bypass this behavior.

Most source files were still untracked at handoff. Inspect `git status` before
editing; the working tree is the current implementation, not a disposable
scratch directory. No commit or push was requested.

## First actions and completion gates

1. Read this document, README and the code owning SSH/profile/process behavior.
   Run `make test` and `make check`. At handoff, 14 tests passed and Clippy was
   clean. Finish when the existing baseline is understood and reproduced.
2. Establish a reachable Linux target. A previously configured destination timed out; this is not a verified working target.
   Inspect available SSH configuration before asking for information. Finish
   when a real SSH command succeeds, or report the specific missing access.
3. Inventory the actual X server and usable capture/input extensions. Record
   whether it is Xorg, Xvfb or Xwayland, the authenticated display, X server
   version and extension versions. Finish when one real application's windows
   can be enumerated using that authenticated connection.
4. Prove one-window capture, display and input over SSH. This is the first
   implementation milestone below. Finish with a real demo and recorded limits.

Without host access, implement and exercise protocol parsing, native rendering
fixtures and Linux build/test infrastructure. Label that evidence correctly;
fixtures cannot pass the remote integration milestone.

## First implementation milestone

Build one complete path for an existing remote X11 session:

```text
Linux X11 application
    -> Rust agent: enumerate + capture one client window
    -> framed binary output over SSH stdio
    -> Rust Mac client: native NSWindow + pixel renderer

Mac key/pointer events
    -> framed input over the same SSH connection
    -> Rust agent: target focus + X11 input injection
```

Acceptance:

- From Seams, select/configure the remote X display and launch or select one
  X11 application. Its content appears in its own Mac window, without the
  Linux desktop or an embedded terminal.
- Window title and actual captured dimensions are correct. Keyboard, pointer,
  button and scroll events affect the intended remote window.
- A second remote window cannot silently receive input meant for the first.
- Resizing the Mac viewport letterboxes/scales consistently; pointer mapping
  accounts for content offsets, remote pixels and Retina backing scale.
- Remote window destruction closes the corresponding Mac window. Closing
  the manager leaves both the menu bar and remote window alive.
- Transport/frame failures produce actionable diagnostics and bounded resource
  use. Disconnect releases held keys/buttons.
- Demo requires no xpra or XQuartz on the Mac. An installed third-party video
  command is not an acceptable hidden requirement.

Start with a simple X11 test app, then a browser explicitly running its X11
backend. Record refresh rate, bytes transferred and observed latency. This
milestone does not require clipboard, audio, file transfer, adoption of Linux
geometry or a production codec.

## Capture and input design

Use the existing Linux X server initially. This avoids building an X server or
requiring a second desktop just to prove capture. Remote app launch must use
the same authenticated `DISPLAY`/`XAUTHORITY` context as the agent. SSH shells
often lack both. Require explicit selection when discovery is ambiguous;
never disable X authentication or use `xhost +`.

Evaluate `x11rb` with only required extension features as a candidate for X11
access. Compare its dependency tree with system-library FFI before choosing.
A small protocol crate is preferable to an unsafe, incomplete X11
implementation. Do not add Tokio or a provider abstraction without a concrete
need. Keep platform-specific code out of shared protocol modules.

Use Composite-backed per-window pixmaps where available. Account for visual
depth, image byte order, row stride and channel masks rather than assuming
every X image is tightly packed BGRA. Define one canonical wire pixel format
and convert at the Linux boundary. Reacquire backing pixmaps on resize/map
changes. Make damage handling and resource destruction explicit.

Use Damage events for steady-state capture after the first full frame. Output
crop is not equivalent to per-window capture: occluded content cannot be
recovered from composed output pixels. Minimized/unmapped windows may have no
useful new content; define their pause/resume behavior.

Enumerate client windows using appropriate X11/WM metadata. Treat transient
dialogs, override-redirect popups and menus as explicit cases; they may not
appear in the usual client list. Avoid exposing WM decoration frames as extra
applications. Native Mac shadows can be drawn locally.

Use XTEST where supported, with explicit targeting/focus and tested keymap
semantics. X11 focus and synthetic events are global to the relevant server,
not isolated per application. Handle key releases on focus loss/disconnect.
Physical keycodes, Unicode text, dead keys and IME composition need separate
design; do not equate macOS keycodes with X11 keycodes.

Omarchy may expose Xwayland instead of a full Xorg desktop. That still limits
this engine to X11 clients. Verify Composite, focus, geometry and input behavior
on the actual server. Xwayland does not make native Wayland apps capturable.
If the required behavior is absent, report the incompatibility and evaluate a
dedicated X server session later. Do not silently revert to xpra.

## Transport and native rendering

Keep the system OpenSSH executable as the trust boundary. Preserve aliases,
user/port/key overrides, SSH-agent support and strict host key verification.
Keep passwords out of profiles, arguments and logs. SSH destination and remote
command quoting already have tests worth preserving.

Create a documented, versioned binary frame format. Use explicit fixed-width
integer encoding, message and payload limits, checked dimensions/stride and
complete reads. Include session generation and window identity so recycled
XIDs or reconnects cannot apply stale frames/input to a different window.

Initial messages should cover handshake/capabilities, window metadata and
destruction, complete pixel frames, input, pause/resume and errors. Add damage
rectangles and compression after complete frames work. Keep debug logs on
stderr; stdout is exclusively protocol data. Test truncation, overflow,
unsupported versions, invalid window IDs and oversized payloads.

Bound frame queues and coalesce superseded work so old video cannot delay
input indefinitely. Complete-frame dropping is straightforward; dropping delta
frames needs an explicit resynchronization/full-refresh rule. Blocking SSH
reads/writes must not run on the AppKit main thread.

For the Mac spike, use native bitmap/Core Graphics rendering through Rust
bindings and AppKit views. Make buffer ownership and main-thread updates
explicit. Keep the menu bar shell independent of remote content windows.

Uncompressed 2560x1440 BGRA at 30 fps is about 442 MB/s before overhead,
exceeding gigabit Ethernet. Raw frames are only a correctness spike. Choose and
measure a compressed path before making performance claims. Apple frameworks
can supply Mac decode; Linux encoding availability remains a design decision.
Do not assume hardware encoders are present or claim a static Linux binary
while dynamically depending on codec libraries.

## Lifecycle and reconnect

Existing X11 apps can outlive an SSH capture process. Agent-owned application
sessions and reconnect state require an explicit owner independent of a broken
SSH channel. Define that lifecycle before claiming the current Reconnect
control works with the new backend.

A later persistent per-user agent can expose a private Unix socket, reached
through an SSH stdio bridge. No listening TCP port is needed. Preserve peer
authentication and private socket permissions. Keep capture paused when no
client subscribes; ensure app launch does not duplicate windows on reattach.

Distinguish disconnect, close one remote window, terminate an agent-owned
session and quit Seams. Never translate a close/stop action into killing the
user's existing desktop session. Existing-session streaming must scope actions
to tracked applications/windows, with a documented ownership policy.

## Integration after the first demo

Replace xpra-specific session methods with the proven engine. Remove the
renderer executable chooser, xpra prerequisite checks and xpra download links
from the standalone product flow. Preserve profile names and SSH fields, but
introduce a versioned distinction between legacy session IDs and actual remote
X display settings. Never reinterpret the old number 100 as desktop `:100`
without an explicit migration decision.

Add our agent build/install flow over SSH. Identify the remote architecture
and library requirements, check compatibility, and install atomically in the
user's account. Document whether the initial agent uses the existing session
or provisions an isolated X server. Keep system package installation explicit.

Next milestones, in order: window catalogue and lifecycle, reliable text/input,
resize/geometry, clipboard, reconnect, efficient compression, then distribution.
Keep audio, file drag/drop, independent Dock identities, menu export and
Wayland-native capture outside the first milestone.

## Verification and delivery

Keep `make test`, `make check`, `make dev`, `make debug`, `make run` and
`make dmg` working, adapting them as the repo grows. The watcher currently
watches `src/` and `packaging/`; remember agent/shared sources and manifests.

Test shared protocol parsers and geometry/input coordinate transforms without
AppKit. Use real Xvfb/Xorg integration tests for capture and input where
possible. Those tests do not prove Xwayland compatibility. macOS rendering and
menu bar lifecycle need a native UI check. A reachable real Linux machine is
required for the end-to-end acceptance demo.

Record exactly what was tested. Prior native UI checks covered configuration,
validation, persistence and SSH errors, not successful remote rendering. No
frame-rate, input-latency or reconnect performance baseline exists yet.

Local bundle/DMG signing is currently ad hoc. Public signing and notarization
remain separate shipping work. Preserve the `AppSeams.dmg` artifact name.

Deliver code, runnable instructions, measured demo results and explicit
remaining limitations. A settings-only app or an xpra wrapper does not satisfy
the standalone engine assignment.

## Primary references

- [X11 protocol](https://xorg.freedesktop.org/releases/current/doc/xproto/x11protocol.html)
- [X.Org extension overview, including Composite and Damage](https://www.x.org/guide/extensions/)
- [XTEST library specification](https://www.x.org/releases/current/doc/libXtst/xtestlib.pdf)
- [x11rb repository](https://github.com/psychon/x11rb), evaluate before adding
- [NSWindow](https://developer.apple.com/documentation/appkit/nswindow)
- [NSStatusItem](https://developer.apple.com/documentation/appkit/nsstatusitem)
- [Core Graphics](https://developer.apple.com/documentation/coregraphics)
