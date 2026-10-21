# Testing

## Automated

```
cargo test --workspace --exclude mousekey-lan
```

| Suite | What it covers |
|---|---|
| `mkl-protocol` unit | Codec round-trips for every message, compact mouse moves (≤ 8 bytes), Noise handshake mutual authentication, identical pairing codes, ordered delivery of 500 messages, closed-stream detection, **tampered ciphertext rejection** through a byte-flipping relay, pairing-link parsing. |
| `mkl-input` unit (Windows) | Absolute-coordinate normalisation hits exact pixels on multi-monitor desktops with negative origins; monitor enumeration. |
| `mkl-store` unit | Identity created once, device lifecycle (trust, block, unblock), settings and layout persistence, DPAPI round-trip. |
| `mkl-core` unit | Edge resolution (left/right/up/down, mixed resolutions, negative coordinates, partial overlap), corner exclusion, inner-edge suppression, injection authorization and release-all. |
| `mkl-core` **multi-device** (`tests/multi_device.rs`) | Real engines over localhost TCP with mock injectors: pairing approval, edge switch A→B with input forwarded and injected, return to A, **stuck-key release on disconnect**, automatic **reconnection**, declined and **blocked** devices refused without a prompt, **identity pinning** rejects a wrong device, **revocation** propagates, **three-device chain** A→B→C switching. |

## Manual test matrix (two or more Windows PCs)

| # | Scenario | Expected |
|---|---|---|
| 1 | Install on PC A and PC B; open the app on both | Each sees the other under *Pair devices* within ~5 s. |
| 2 | Pair from A; compare codes; approve on B | Both show "Paired"; dashboards show Online with latency. |
| 3 | Arrange B to the right of A, save | B's layout screen shows the same arrangement. |
| 4 | Move the cursor off A's right edge | Cursor appears on B's left edge; typing goes to B. |
| 5 | Ctrl+C / Ctrl+V, Alt+Tab, Win key, Shift+arrows on B | All work on B and have no effect on A. |
| 6 | Scroll and click (left/right/middle/back/forward) on B | All work. |
| 7 | Hold Shift on B, then press Ctrl+Alt+Home | Control returns to A; no stuck Shift on either PC. |
| 8 | Pull B's network cable while controlling it | Within 5 s control returns to A; B releases keys; reconnects after the cable is back. |
| 9 | Lock B (Win+L on B's own keyboard) while controlling it | A takes control back; B shows "Locked"; after unlocking B, edge switching works again. |
| 10 | Sleep B and wake it | Reconnects within a few seconds of resume. |
| 11 | Restart B | After sign-in on B, the tray icon appears and the connection returns automatically. |
| 12 | Different DPI (100 % vs 150 %) and resolutions | Crossing point matches the layout; no drift. |
| 13 | Third PC C to the right of B | A → B → C chain works; Ctrl+Alt+Home returns to A. |
| 14 | Forget B on A | B shows "no longer trusts this PC"; no reconnection. |
| 15 | Unknown PC D tries to pair; Block | No further prompts from D. |

## Performance targets and how to measure

Open **Diagnostics** on the controlling PC:

| Metric | Target | Where |
|---|---|---|
| Hook processing time | < 20 µs avg, < 1 ms max | Hook latency card |
| Network round trip | < 5 ms wired, < 15 ms Wi-Fi | Device table |
| CPU while idle | < 0.5 % | App resources card |
| CPU while moving the mouse | < 3 % | App resources card |
| Memory | < 80 MB working set (mostly WebView2; the engine itself is a few MB) | App resources card |
| Reconnection after a link drop | < 3 s after the network is back | Reconnects card |
| Dropped events | 0 on a healthy LAN | Device table |
