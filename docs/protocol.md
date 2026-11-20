# Window transport

Seams uses the sibling bridge's Rust client for capability negotiation, frame validation and input messages. The bridge owns the [wire protocol](../../x11-bridge/docs/protocol.md).

The launchpad runs the agent's `--desktop list` over system SSH. Choosing a desktop entry starts `--desktop open TOKEN ID`. The agent returns the verified existing windows for the selected app when available. Otherwise it launches the app and reports windows belonging to the new launch. Seams opens a separate `--desktop attach-app TOKEN XID ID` connection for each window.

Each viewer keeps the latest pending RGBA frame and draws it in an aspect-fit NSImageView. Pointer mapping uses the same content rectangle. SSH compresses the stream. When both peers negotiate resize, the Mac sends the settled content size in logical points and the agent requests that X11 geometry. On Hyprland, the agent uses XRes to identify the managed window’s owner and asks the compositor to float and resize a unique matching Xwayland window. Otherwise it uses ConfigureWindow. The viewer keeps aspect-fitting the latest frame while waiting for Linux to draw at the requested size. Losing focus releases held input. Closing a viewer drops its stream while the remote application continues running.
