# Omarchy Remote Launchpad

Seams is a native macOS launchpad for apps on an SSH-connected Linux desktop. Search by app name or desktop ID, use the arrow keys to select a result, and press Return to launch. Each captured X11 window opens in its own Mac window. The Seams menu bar item brings the launchpad back.

## Run with an existing Linux host

Requires macOS 13+, Rust, Apple command line tools and the sibling x11-bridge checkout with desktop launch support.

1. Build the Linux agent from that checkout with `cargo build --release --locked -p x11-bridge-agent`. Install the executable on the Linux host in `~/.local/bin/x11-bridge-agent` or on its SSH login PATH.
2. Put one SSH alias or `user@host` on a single line in `seams.local` at the root of this checkout. Git ignores this file. Verify that system SSH can connect using keys and an existing trusted host key.
3. Run `make run` for the app bundle or `make dev` for live rebuilds. Refresh reloads the file and app list.

`make dev` appends failed build output, runtime diagnostics and launcher errors to `errors.txt` in the checkout. Git ignores this file. Invoke `$fix-errors` to investigate the recorded failures.

When moved outside the checkout, the app reads `~/Library/Application Support/Seams/seams.local`. Set `SEAMS_CONFIG` to override the file path. The app bundle contains no connection settings.

## Run with a local Omarchy VM

Use an Apple silicon Mac running macOS 15 or newer, Rust, Apple command line tools and a sibling x11-bridge checkout with desktop launch support. [Try Omarchy v1.4.1](https://github.com/omacom/try-omarchy/releases/tag/v1.4.1) provisions a persistent ARM64 Omarchy guest with native CPU virtualization and accelerated graphics. The x86-64 ISO would need CPU emulation on these Macs. Intel hosts and x86 guests are outside this flow. Allow about 15 GB free on APFS for first boot, plus space for guest apps and builds. The runtime defaults to 4 or 8 GiB guest RAM according to host memory and a grow-on-use 64 GiB disk. [Runtime and storage details](https://github.com/omacom/try-omarchy/blob/v1.4.1/manual/updates-and-data.md)

1. Install Try Omarchy from its release DMG in Applications. From the Seams checkout, run `scripts/local-vm start`. Choose **Launch Omarchy** to download the image. Complete first boot, create your Linux account and log in to the desktop.
2. In Try Omarchy settings, use shared networking. Under **Network → Port Forwarding**, select **Add SSH**, save, and restart the guest to apply the change. This forwards Mac loopback port 2222 to guest port 22. Log in again. [Upstream SSH instructions](https://github.com/omacom/try-omarchy/blob/v1.4.1/manual/files-network-devices.md)
3. On the Mac, run `scripts/local-vm setup YOUR_LINUX_USER`. Append the host port if you changed it. Compare the SSH fingerprint with the one printed by `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` in the guest console. Enter the guest password when prompted. Setup installs a dedicated SSH key, builds the agent from the same sibling x11-bridge checkout used by Seams, and installs it under `~/.local/bin` in the guest. The guest may prompt for permission to install build tools and `xdpyinfo`. Setup configures Hyprland to scale Xwayland apps with the guest desktop so a resize in Mac points produces the requested X11 bitmap size on Retina displays.
4. Setup checks Xwayland and reads the app catalogue before saving the target. Run `make run`, or press **Refresh** in an open Launchpad. Search for `lstopo` (Hardware Locality), press Return and resize its Mac window. The guest draws the hardware topology at the new size.

After a restart, choose **Open Local VM…** in the Seams menu, launch the same guest, log in and press **Refresh**. Guest apps, profiles and files stay on the VM disk. Run `scripts/local-vm status YOUR_LINUX_USER [PORT]` for a connection check. A refused connection usually means the VM is stopped or the forward needs applying. If the desktop check fails, unlock the graphical session for that account and retry.

Setup stores its SSH key and pinned host keys under `~/.ssh/seams-vm/`. Switching VMs on the same forwarded port changes the server fingerprint. Verify the replacement in its console before removing the old entry with `ssh-keygen -f ~/.ssh/seams-vm/known_hosts -R '[127.0.0.1]:2222'` and repeating setup. To return to an existing SSH target, run `cargo run -- --select-target USER@HOST`, then press **Refresh**. Both target types use the same catalogue check and config file selection described above.

## Bridge behavior

The agent lists desktop entries and opens existing X11 windows whose class and owning executable match the selected entry. Otherwise it starts the selected entry with GIO and identifies its windows by launch token. SSH compresses RGBA frames in transit and carries keyboard and pointer input. Resizing the Mac window requests a new Linux layout. On Hyprland, the agent floats the selected window on resize when it can identify the Xwayland window by its owning process and, when needed, its geometry. The window stays floating after detach. The Linux host needs a running authenticated X11 or Xwayland session with Composite, Damage, XRes and XTEST. The agent can read the desktop environment from the user's systemd manager.

Selecting the standard Chromium entry opens an X11 browser with a persistent profile in `$XDG_DATA_HOME/seams/chromium` (normally `~/.local/share/seams/chromium` on Linux). Its tabs, extensions and sign-ins belong to that profile. Opening Chromium again reconnects to its Seams windows.

When Chromium is the default browser, Seams opens single-URL Omarchy web apps such as X with a separate X11 Chromium profile per desktop entry. Sign-ins persist under `$XDG_DATA_HOME/seams/webapps/<desktop-id>`. Reopening the entry reconnects to its window.

Native Wayland windows and apps that hand off to an existing process can appear in search but fail to produce a capturable window. You can click exposed parts of the selected Linux window while another app has keyboard focus. Typing requires the selected window to hold Linux keyboard focus. Closing a Mac viewer detaches it and leaves the Linux app running.

## Verify

- `make check` checks formatting and runs Clippy.
- `make test` runs unit tests.
- `./scripts/test-native` launches an isolated Xvfb app through Docker and SSH. It checks AppKit pixels, keyboard delivery, dialog capture, remote resize and pointer accuracy.
- With a local VM selected, `SEAMS_TEST_DESKTOP_ID=lstopo.desktop SEAMS_TEST_TILED_RESIZE=1 cargo test --test native --locked` checks the stock X11 app at two Mac window sizes. Repeat after shutting down and starting the guest to check reconnect.
- `SEAMS_TEST_DESKTOP_ID=<desktop-id> cargo test --test native --locked` launches that installed app on the configured host and checks for a visible AppKit image.
