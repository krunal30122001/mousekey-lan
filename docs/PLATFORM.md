# Platform notes: permissions, lock screen and sign-in

This document records what each operating system allows, what MouseKey LAN does today, and
what it deliberately does **not** do. Rule: no authentication bypass, no password capture, no
circumvention of secure-desktop protections.

## Windows (supported: Windows 10 1809+ and Windows 11, x64)

### How input is captured and injected

| Function | API | Notes |
|---|---|---|
| Capture | `SetWindowsHookEx(WH_MOUSE_LL / WH_KEYBOARD_LL)` on a dedicated thread | Runs in the user's session. Events can be swallowed while another PC is being controlled. Injected events (`LLMHF_INJECTED`) are ignored, which prevents feedback loops. |
| Injection | `SendInput` (absolute `MOUSEEVENTF_VIRTUALDESK` moves, scan-code keys) | Scan codes keep keys layout-independent. |
| Displays | `EnumDisplayMonitors` + per-monitor DPI awareness v2 | All coordinates are physical pixels, so mixed scaling (100 % / 150 %) works. |
| Lock / unlock | `WTSRegisterSessionNotification` → `WM_WTSSESSION_CHANGE` | Hidden top-level window on the hook thread. |
| Sleep / resume | `WM_POWERBROADCAST` | Releases held input before sleep; reconnects immediately on resume. |
| Display changes | `WM_DISPLAYCHANGE` | Re-enumerates monitors and sends them to peers. |

No administrator rights are needed for normal use. The installer is per-machine (it asks for
elevation once) so that it can add a Windows Firewall rule:

```
netsh advfirewall firewall add rule name="MouseKey LAN" dir=in action=allow program="<install dir>\mousekey-lan.exe" enable=yes profile=any
```

The app listens on **TCP 47820** and uses **mDNS (UDP 5353)** for discovery.

### Known Windows restrictions (by design)

| Situation | What happens | Why |
|---|---|---|
| **Elevated (admin) windows** on the *controlled* PC | Input is ignored while an elevated window has focus. | User Interface Privilege Isolation (UIPI) blocks a medium-integrity process from injecting into high-integrity windows. Fix: **Settings → Security → Run as administrator** on that PC (registers an elevated logon task via one UAC prompt). Without it, the controlling PC gets a warning the first time typing is blocked. |
| **Elevated windows** on the *controlling* PC | Low-level hooks may not see input while an elevated window has focus. | Same UIPI rule. |
| **Ctrl+Alt+Del** | Never forwarded. | The Secure Attention Sequence is handled by the kernel/Winlogon and can't be hooked or synthesized by applications. |
| **Win+L** on the controlling PC | Locks the controlling PC, not the remote one. | Windows handles Win+L before hooks can block it. |
| **UAC prompts** (secure desktop) | Can't be clicked or typed into remotely. | UAC runs on the isolated Winlogon secure desktop. |

### Lock screen

The lock screen (LogonUI) runs on the **Winlogon secure desktop**. Processes in the user's
session, including any regular Tauri app, can't hook input there or inject into it. This is a
Windows security boundary, not a bug.

What MouseKey LAN does:

1. **Controlling PC locks:** the hooks stop receiving input; MouseKey LAN returns control
   locally, releases every key and button it was holding on the remote PC, and tells peers it
   is locked. Connections stay open.
2. **Controlled PC locks:** it releases every injected key and button, reports
   `Status{locked:true}`, and the controlling PC immediately takes control back (the cursor
   would otherwise vanish into a screen that ignores input). Dashboard shows "Locked".
3. **Unlock:** sharing resumes immediately. No reconnection is needed because the encrypted
   sessions survived the lock. Monitors are re-enumerated in case they changed.

### Sign-in screen (after reboot or sign-out)

Before a user signs in, there is no user session and the app (a per-user, autostarted
process) isn't running. **Input sharing before sign-in is not supported.** Sign in on that PC
with its own keyboard; MouseKey LAN starts (autostart via `HKCU\...\Run`) and reconnects to
trusted devices within a few seconds.

#### Research: what would an officially supported pre-login mode involve?

| Approach | Status | Assessment |
|---|---|---|
| Per-user app (current) | Implemented | Can't reach the secure desktop. Safe. |
| **Windows service (LocalSystem) + helper launched into the console session on `winsta0\winlogon`** via `WTSGetActiveConsoleSessionId`, a duplicated SYSTEM token and `CreateProcessAsUser` | Not implemented | Uses documented APIs; it is how Microsoft PowerToys' Mouse Without Borders "service mode" works. It means a SYSTEM-privileged process on the secure desktop that accepts input from the network. That is a large attack surface: a compromised paired device could drive UAC prompts and the sign-in UI. If added, it must be opt-in, admin-installed, accept input only from already-paired keys, never read or store credentials, and log every secure-desktop session. Planned as a separate, optional `mkl-service` crate. |
| `SendSAS` (Ctrl+Alt+Del) from a service | Not implemented | Requires the "Disable or enable software Secure Attention Sequence" group policy. Only meaningful together with the service mode above. |
| Credential Provider | Not applicable | Credential providers add *authentication methods*; they aren't an input-forwarding mechanism. Misusing one to inject input would be inappropriate. |
| Auto sign-in (Sysinternals Autologon) | User choice | Avoids the sign-in screen entirely, but stores credentials (LSA-encrypted) and weakens physical security. Not recommended. MouseKey LAN never configures it. |
| Turning off the secure desktop for UAC (`PromptOnSecureDesktop=0`) | Not recommended | Weakens protection against spoofed elevation prompts. MouseKey LAN never changes it. |

### Startup timing

* **Application restart:** pairings, layout and settings load from SQLite; trusted devices are
  re-dialed immediately (lower device id first, other side after 1.5 s to avoid duplicate
  connections).
* **System restart:** the earliest supported user-mode stage is *user sign-in*, through
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` with `--hidden` (tray only). Earlier
  stages need the service mode described above.
* **Network changes / Wi-Fi drop:** heartbeats every 1 s and a 5 s silence timeout detect dead
  links; reconnects back off exponentially from 0.3 s to 30 s and reset on discovery, unlock,
  resume or disconnect events.

## macOS (not yet implemented)

* Capture: `CGEventTap` (needs **Accessibility** and, on macOS 10.15+, **Input Monitoring**
  permission). Injection: `CGEventPost` (Accessibility).
* Lock screen / login window: protected by the system (`loginwindow`); apps can't inject.
  Secure Input mode (password fields) also blocks event taps. Not supported.
* Autostart: `SMAppService` login item (`tauri-plugin-autostart` uses a LaunchAgent).
* Implementation goes in `crates/mkl-input/src/mac.rs` behind `cfg(target_os = "macos")`.

## Linux (not yet implemented)

* **X11:** XInput2 raw events for capture, XTest for injection.
* **Wayland:** global capture and injection aren't allowed by design. Use the
  `org.freedesktop.portal.RemoteDesktop` / InputCapture portals and `libei` where the
  compositor supports them (GNOME 45+, KDE 6.1+). `/dev/uinput` injection needs root or a
  udev rule and bypasses the compositor's security model, so it's not used.
* Greeter (GDM/SDDM) runs as a separate user and session: not supported.

## Input hook watchdog

Windows silently removes low-level hooks whose callback exceeds `LowLevelHooksTimeout`. The
hook thread runs a 2 s timer that compares `GetLastInputInfo` (the last input the system saw)
with the time of the last hook callback; if the system saw input more than a second after
the hooks last ran, both hooks are reinstalled and a warning is logged. Cursor moves are
never performed while the router lock is held, because `SetCursorPos` can feed back into the
hook, which takes that lock.
