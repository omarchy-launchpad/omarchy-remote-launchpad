# Protocol groundwork

This module is not connected to the existing xpra UI. No native rendering or
Linux capture has been demonstrated. The previously configured target returned an SSH port 22 timeout on 2026-11-15 with strict host verification.

The 32-byte header contains magic SEAM, u16 version 1, u16 message kind,
u32 payload length, u32 reserved zero, u64 session generation and u64 window
identity. All integers are little endian. Generation must be nonzero. Window
identity must be nonzero except for Hello and Error. The eventual agent must
assign identities independent of recycled XIDs and reject stale generations.
Payloads are bounded to 32 MiB before allocation. EOF between packets is clean;
partial headers or bodies fail. Unknown kinds and versions fail.

Kinds are Hello=1, Window=2, Destroy=3, Frame=4, Input=5, Pause=6, Resume=7,
Error=8. Destroy, Pause and Resume have empty payloads. Frame payloads contain
u32 width, u32 height, then tightly packed top-to-bottom RGBA8 pixels. Other
message payload schemas remain undefined and are not ready for interoperability.

Pointer mapping accepts top-left-origin viewport coordinates in logical points.
Rendering must use the same aspect-fit rectangle. AppKit bottom-left coordinates
must be flipped before mapping. Letterbox margins reject input. Retina scale
must not be applied again to logical point coordinates.

Validation: make test and make check pass. Existing 14 tests are preserved;
three new tests cover framing, every truncation boundary, hostile headers,
invalid dimensions and letterbox mapping. Remaining work includes Linux agent,
native rendering fixtures, input bridge, transport lifecycle, settings migration
and real remote acceptance demo. No performance measurements exist.
