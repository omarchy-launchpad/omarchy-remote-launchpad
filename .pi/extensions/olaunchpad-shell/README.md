# olaunchpad-shell

Project-local Pi extension for running and testing Omarchy Remote Launchpad in development.

It starts `make dev` only on demand via `/olaunchpad start`, `/olaunchpad show`, or the `olaunchpad_shell` tool, then owns that watcher until the session shuts down. Do not start a second watcher.

## Interactive command

Run `/olaunchpad` to open the floating Launchpad Control Center, or use:

- `/olaunchpad status`
- `/olaunchpad show`
- `/olaunchpad launch Chromium`
- `/olaunchpad logs`
- `/olaunchpad errors`
- `/olaunchpad restart`
- `/olaunchpad stop`
- `/olaunchpad start`
- `/olaunchpad troubleshoot`

`logs`, `errors`, `status`, and `troubleshoot` open in a scrollable overlay. Use arrow keys or `Ctrl+U`/`Ctrl+D` to scroll and Escape to close. Launch automation uses macOS System Events; the terminal running Pi may need Accessibility permission.

## Agent tool

The extension registers `olaunchpad_shell` with the same actions. The agent can manage the one hosted `make dev` process, launch applications by name or desktop ID, inspect current captured output, compare it with the append-only `errors.txt`, and restart after changes.

Tool calls render as compact, action-aware status rows. Multiline output stays collapsed by default and can be opened with Pi's tool-expand key.

The complete captured watcher output is stored in a project-specific temporary file under `$TMPDIR`; `status` reports its path.
