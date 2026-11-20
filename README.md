# Omarchy Remote Launchpad

Seams is a native macOS launchpad for apps on an SSH-connected Linux desktop. Search by app name or desktop ID, use the arrow keys to select a result, and press Return to launch. Each captured X11 window opens in its own Mac window. The Seams menu bar item brings the launchpad back.

## Run

Requires macOS 13+, Rust, Apple command line tools and the sibling x11-bridge checkout with desktop launch support.

1. Build the Linux agent from that checkout with `cargo build --release --locked -p x11-bridge-agent`. Install the executable on the Linux host in `~/.local/bin/x11-bridge-agent` or on its SSH login PATH.
2. Put one SSH alias or `user@host` on a single line in `seams.local` at the root of this checkout. Git ignores this file. Verify that system SSH can connect using keys and an existing trusted host key.
3. Run `make run` for the app bundle or `make dev` for live rebuilds. Refresh reloads the file and app list.

`make dev` appends failed build output, runtime diagnostics and launcher errors to `errors.txt` in the checkout. Git ignores this file. Invoke `$fix-errors` to investigate the recorded failures.

When moved outside the checkout, the app reads `~/Library/Application Support/Seams/seams.local`. Set `SEAMS_CONFIG` to override the file path. The app bundle contains no connection settings.

## Bridge behavior

The agent lists desktop entries and opens existing X11 windows whose class and owning executable match the selected entry. Otherwise it starts the selected entry with GIO and identifies its windows by launch token. SSH compresses RGBA frames in transit and carries keyboard and pointer input. Resizing the Mac window requests a new Linux layout. On Hyprland, the agent floats the selected window on resize when it can identify the Xwayland window by its owning process and, when needed, its geometry. The window stays floating after detach. The Linux host needs a running authenticated X11 or Xwayland session with Composite, Damage, XRes and XTEST. The agent can read the desktop environment from the user's systemd manager.

Selecting the standard Chromium entry opens an X11 browser with a persistent profile in `$XDG_DATA_HOME/seams/chromium` (normally `~/.local/share/seams/chromium` on Linux). Its tabs, extensions and sign-ins belong to that profile. Opening Chromium again reconnects to its Seams windows.

When Chromium is the default browser, Seams opens single-URL Omarchy web apps such as X with a separate X11 Chromium profile per desktop entry. Sign-ins persist under `$XDG_DATA_HOME/seams/webapps/<desktop-id>`. Reopening the entry reconnects to its window.

Native Wayland windows and apps that hand off to an existing process can appear in search but fail to produce a capturable window. You can click exposed parts of the selected Linux window while another app has keyboard focus. Typing requires the selected window to hold Linux keyboard focus. Closing a Mac viewer detaches it and leaves the Linux app running.

## Verify

- `make check` checks formatting and runs Clippy.
- `make test` runs unit tests.
- `./scripts/test-native` launches an isolated Xvfb app through Docker and SSH. It checks AppKit pixels, keyboard delivery, dialog capture, remote resize and pointer accuracy.
- `SEAMS_TEST_DESKTOP_ID=<desktop-id> cargo test --test native --locked` launches that installed app on the configured host and checks for a visible AppKit image.
