# Architecture

```
mousekey-lan/
├─ crates/
│  ├─ mkl-protocol   messages + binary codec, Noise XX transport, pairing codes/links
│  ├─ mkl-input      CaptureSink / InputInjector traits; Windows hooks + SendInput (win.rs)
│  ├─ mkl-store      SQLite: identity key (DPAPI), devices, layout, settings
│  ├─ mkl-discovery  mDNS / DNS-SD advertise + browse
│  └─ mkl-core       engine: sessions, pairing, router (edge switching), injection, reconnect
├─ src-tauri/        desktop shell: commands, tray, autostart, logging, metrics
└─ src/              Vue 3 + TypeScript + Tailwind UI
```

Dependencies only point downward: `src-tauri → mkl-core → {protocol, input, store, discovery}`.
The UI never touches the input path. It only reads snapshots and sends commands.

## Hot path (latency-critical)

```
OS low-level hook (dedicated thread, own message loop)
   └─ router::handle_input            ~1-5 µs, no allocation for moves
        ├─ Local mode: pass through; detect outer-edge hit → layout.resolve → ControlEnter
        └─ Remote mode: swallow; convert to delta from anchor; track remote cursor
              └─ PeerConn.tx.try_send (bounded 512; never blocks the hook)
                    └─ writer task: batch up to 64 msgs → 1 Noise-encrypted write (TCP_NODELAY)
                                                   ⇣ LAN
reader task → decrypt → dispatch → InjectState.apply (validate, track pressed) → SendInput
```

* **Never blocks the hook.** If the queue is full (stalled link), mouse moves are dropped (they
  are absolute, so the next one corrects position) and counted in diagnostics.
* **No polling.** Everything is event-driven (hooks, sockets, notifications). The reconnect
  loop wakes every 2 s only to check backoff timers, and only dials offline trusted devices.
* **Frontend not involved.** Input never crosses the Tauri IPC boundary.

## Control model

* Each PC both captures (it can control others) and injects (it can be controlled).
* The PC whose physical mouse is moving is the *controller*. While controlling, its cursor is
  parked at the centre of its primary monitor and movement deltas drive a virtual cursor in
  the target's coordinate space.
* Crossing back over an edge that leads to this PC returns control. Crossing an edge that
  leads to a third PC switches directly (A → B → C chains).
* Escape hatches: **Ctrl+Alt+Home**, the tray menu, the dashboard button, locking the PC,
  disconnects, and physical input on the controlled PC (it reclaims control).

### Stuck-key prevention

* The controller tracks every key and button pressed on the remote and sends releases on
  every exit path (edge, hotkey, switch, disconnect, lock, sleep, shutdown).
* The receiver independently tracks everything it injected and releases it on
  `ControlLeave`, a controller change, disconnect, lock, sleep, or app exit.
* Keys held locally when control leaves have their key-ups delivered locally (passthrough),
  so the controlling PC doesn't end up with a stuck Ctrl/Shift either.
* Switching is suppressed while a mouse button is held (no half-drags across machines).

## Layout

Every device has a position in a shared pixel space. Monitors keep their real relative
arrangement inside the device's bounding box. `Layout::resolve(from, point, direction)` finds
the nearest monitor of another device beyond that edge that overlaps on the perpendicular
axis (gap ≤ 600 px) and maps the point into the target's own coordinates (2 px inset).

The layout is replicated to peers with a revision number:
* adopt a peer's layout if it covers every device we've placed and is newer (ties go to the
  lower device id);
* if a peer knows strictly fewer devices, keep ours, bump the revision and send it back.

## Connection lifecycle

1. TCP connect (3 s timeout), `TCP_NODELAY`.
2. **Noise `XX_25519_ChaChaPoly_BLAKE2s`** handshake with static device keys (6 s timeout).
3. Device id = first 8 bytes of SHA-256(static key). The `Hello` must match it.
4. Authorization: both sides must send `Accept`. A side accepts if the key is trusted, if its
   own user initiated the pairing, or if its user approves the prompt (120 s).
5. Session: writer task, heartbeat task (1 s ping, RTT measurement), reader loop with a 5 s
   silence timeout.
6. Duplicate connections (both sides dial at once) are resolved deterministically: a reconnect
   from the same dialer replaces a stale session; otherwise the lower id's dial wins.

## Persistence (SQLite, `%APPDATA%\com.mousekeylan.app\mousekey.db`)

| Table | Contents |
|---|---|
| `identity` | Device static keypair (private key DPAPI-encrypted) |
| `devices` | id, name, public key, trusted, blocked, last address, last seen, screens |
| `layout` | device id → position |
| `settings` | JSON settings, layout revision |

No input data is ever stored or logged.
