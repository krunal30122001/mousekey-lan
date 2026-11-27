# MouseKey LAN

Control several Windows PCs on the same Wi-Fi or LAN with **one keyboard and mouse**. Push the
cursor past a screen edge and it continues on the next computer, with keyboard focus following
it. Inspired by Mouse Without Borders and Synergy. Built with Rust, Tauri 2 and Vue 3.

* Seamless edge switching with a visual, drag-and-drop **monitor layout** (mixed resolutions,
  multiple monitors, any arrangement, 3+ PCs)
* Mouse moves, all buttons, vertical/horizontal scroll, keyboard and shortcuts (scan codes, so
  keyboard layouts don't matter)
* Automatic LAN **discovery** (mDNS), plus pairing by IP address, link or QR code
* **Mutual pairing approval** with a 6-digit verification code; end-to-end **encryption**
  (Noise XX: X25519 / ChaCha20-Poly1305)
* Auto-start at sign-in, tray icon, automatic **reconnection** after network drops, sleep,
  lock and restart
* **Shared clipboard** (text and images), **copy/paste files** between PCs, and **drag & drop**
  files across the screen edge (saved to `DownloadsMouseKey LAN` on the other PC)
* Stuck-key protection, emergency return hotkey **Ctrl+Alt+Home**
* Diagnostics: latency, connection quality, hook timing, CPU/memory, reconnect time

## Install (each PC)

1. Copy `MouseKey LAN_0.3.1_x64-setup.exe` to the PC and run it.
   * Windows SmartScreen may say *"Windows protected your PC"* because the build isn't
     code-signed yet. Click **More info → Run anyway**.
   * Approve the administrator prompt. The installer adds a Windows Firewall rule for the app.
2. MouseKey LAN opens and starts automatically at every sign-in (it lives in the tray).
3. Requirements: Windows 10 (1809+) or 11, x64, WebView2 runtime (preinstalled on Windows 11;
   the installer fetches it on Windows 10 if missing).

## Pair two PCs

1. On PC **A** open **Pair devices**. PC **B** appears under *Computers on this network*
   (or type B's IP address, shown on B's Pair devices page).
2. Click **Pair** on A. B shows a request with a 6-digit code. Check that A shows the
   **same code**, then click **Codes match, approve** on B.
3. Open **Monitor layout** and drag the PCs to match your desk. Click **Save layout**.
4. Move the mouse off the edge where the PCs touch. 🎉

**Ctrl+Alt+Home** always brings control back to the PC you're physically using.

## Known limitations (Windows security boundaries)

* The **lock screen** can be controlled only with the optional lock-screen service
  (Settings → Security, needs *Run as administrator*); see [docs/LOCKSCREEN.md](docs/LOCKSCREEN.md).
  Without it, control returns automatically when a controlled PC locks; unlock it with its own
  keyboard, and sharing resumes by itself. The **sign-in screen** after signing out, **UAC
  prompts** and **Ctrl+Alt+Del** can't be controlled remotely. See
  [docs/PLATFORM.md](docs/PLATFORM.md) for the research and options.
* Windows ignores input sent into **apps running as administrator** unless MouseKey LAN also
  runs as administrator on that PC.
* After a reboot, sharing starts once a user signs in on that PC.

## Build from source

Prerequisites: Rust (stable, MSVC), Node.js 20+, Visual Studio Build Tools (C++), WebView2.

```bash
npm install
npm run tauri dev        # development with hot reload
npm run tauri build      # release: target/release/bundle/nsis/*-setup.exe
cargo test --workspace --exclude mousekey-lan
```

Logs are written to `%APPDATA%\com.mousekeylan.app\logs\mousekey.log` (`MKL_LOG=debug` for more detail).

## Documentation

* [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, hot path, control model, layout, protocol
* [docs/SECURITY.md](docs/SECURITY.md): threat model and pairing
* [docs/PLATFORM.md](docs/PLATFORM.md): Windows permissions, lock screen and pre-login research, macOS/Linux plan
* [docs/TESTING.md](docs/TESTING.md): automated tests, manual test matrix, performance targets
