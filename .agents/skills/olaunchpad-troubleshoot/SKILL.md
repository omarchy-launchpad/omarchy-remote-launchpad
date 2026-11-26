---
name: olaunchpad-troubleshoot
description: Diagnose and, when possible, fix failures recorded by make dev.
disable-model-invocation: true
---

# Olaunchpad troubleshooting

Treat `errors.txt` as evidence, not a work queue: `make dev` appends across rebuilds and sessions, so old failures can describe code that no longer exists.

## Diagnose

1. From the repository root, read `scripts/dev-macos`, `git status --short`, and the tail of `errors.txt`. If the user supplied a symptom or time, use it to select the relevant lines. Otherwise select the newest actionable signature; ask what failed only when the tail contains multiple plausible runtime incidents.
2. Collapse identical repeated messages and rebuild blocks into one incident. Locate the incident boundary from build-failure timestamps, process-exit timestamps, and ordering. Runtime diagnostics are not timestamped, so state uncertainty rather than assigning them to a session without evidence.
3. Classify the incident and trace it to its producer:
   - **build** — Rust diagnostics and the source/Cargo inputs they name;
   - **local lifecycle/UI** — `src/main.rs`, launcher, viewer, unlock, or dev-reload behavior;
   - **SSH/agent** — `src/remote.rs`, `omarchy-launchpad.local`, system SSH, and the sibling `../x11-bridge` checkout;
   - **window capture/input/video** — the window stream plus the sibling agent/protocol implementation.
4. Test the leading hypothesis with the narrowest non-destructive probe. Prefer current source inspection and focused tests. Use `cargo build --locked --bin omarchy-launchpad` for a build incident. Use read-only SSH or remote-agent probes for transport incidents. Preserve the running `make dev` process; do not start another watcher or app instance merely to reproduce a log line.
5. Distinguish cause from noise. In particular:
   - `Blocking waiting for file lock` is contention unless it persists or ends in failure.
   - `omarchy-launchpad is already running` means a second launch handed off to the existing instance.
   - `Build failed. Keeping previous app running` reports fallback behavior; the preceding compiler diagnostic is the cause.
   - Native Wayland, single-instance, obscured-window, route-to-host, and decoder messages are separate failure classes even when adjacent.
6. Compare every historical diagnostic with the current tree before changing code. A successful focused probe retires that old incident; continue only if a newer actionable incident remains.

## Resolve

When the cause is in this checkout and the requested troubleshooting implies a fix, make the smallest source change that addresses the demonstrated cause while preserving unrelated work. For host state, credentials, networking, or software on the Linux machine, report the exact failing boundary and the next safe command instead of guessing or changing remote state.

Verify a code fix with the focused reproducer, then run the relevant repository check (`make check`, `make test`, or the narrower test when full validation needs unavailable remote infrastructure). Do not truncate or rewrite `errors.txt`; it is live append-only output from the watcher.

Finish only when the root cause is supported by both the selected log evidence and a source trace or reproducer. Report: incident selected, root cause, changes made, verification, and any remaining external blocker. If the evidence is insufficient, report the competing hypotheses and request the single observation that separates them.
