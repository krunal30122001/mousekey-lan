# Lock screen and sign-in support: audit, options and plan

Status: Phase 1 is implemented. Phase 2 (a privileged service for the lock/sign-in screen) is not being built.


## 1. Audit: what happens today when PC 2 locks

Read from the code (`router.rs::handle_system`, `net.rs` `Message::Status`, `win.rs`, `main.rs`):

| Moment | What the code does | Result |
|---|---|---|
| PC 2 locks (Win+L, timeout) | Per-user app gets `WM_WTSSESSION_CHANGE` → sets `locked`, releases injected keys/buttons, ends control, broadcasts `Status{locked:true}` | **Connections stay up.** The controller returns control to PC 1 and refuses to re-enter while `remote_locked` is set (`router.rs` `enter_remote` / `switch_remote`). |
| PC 2 is on the lock screen | The app keeps running on the *user's* desktop. Windows moves keyboard/mouse to the **Winlogon secure desktop**; our hooks and `SendInput` are bound to the user desktop and see/affect nothing there | Nothing can be forwarded. This is a design limit of a per-user process, not a networking fault. |
| PC 2 unlocks | `Status{locked:false}`, screens re-read, backoff cleared, immediate reconnect attempt | Sharing resumes with no restart (already working). |
| PC 2 restarts | No user session → the per-user app isn't running (autostart is `HKCU\...\Run` or a logon task, both fire at **sign-in**) | PC 2 is unreachable until someone signs in. |

**Root cause of "I can't do anything when it's locked":** the code is a per-user process on the
interactive desktop. It never disconnects on lock. It correctly *stands down* because it cannot
reach the desktop that Windows shows while locked. Unlocking is also never automatic.

**One real bug found during this audit (fixed, Phase 1):** the 0.3.0 hook watchdog compared
`GetLastInputInfo` (which counts physical typing at the lock screen) with the last hook
callback. While locked it could conclude the hooks died and reinstall them every 2 s. It now
stands down while the session is locked and re-baselines on unlock/resume.

## 2. Supported Windows architectures compared

| # | Architecture | Reaches lock/sign-in screen? | Admin windows? | Privilege / risk | Verdict |
|---|---|---|---|---|---|
| A | Per-user app (today) | No | No | Lowest | Keep for everyday use |
| B | Per-user app **elevated** via logon task (done in 0.3.0) | No | Yes | Admin token in user session | Keep; solves admin windows |
| C | `uiAccess="true"` signed app in `Program Files` | No | Yes | Needs an Authenticode certificate we don't have | Optional later; same benefit as B |
| D | **LocalSystem service + helper process on the Winlogon desktop** (WTS APIs, `CreateProcessAsUser`, `OpenInputDesktop`/`SetThreadDesktop`, `SendInput`) | **Yes** (lock screen, sign-in screen, and the UAC secure desktop) | Yes | **SYSTEM code that accepts network input** | The documented architecture used by Mouse Without Borders "service mode", Synergy/Deskflow and RustDesk |
| E | Custom credential provider | Not an input mechanism | n/a | Handles credentials | **Rejected** (your rule 7) |
| F | Auto sign-in / disabling Ctrl+Alt+Del or UAC secure desktop | Skips the screen | n/a | Weakens Windows security | **Rejected** (your rule 8) |

**Answer to "can an ordinary per-user app, a service, or another documented component meet the
requirement?"** Only **D** can. A per-user app (A/B/C) cannot, by Windows design. My earlier
reply called this a flat "no"; that overstated it. D is a documented, supported pattern, but it
carries real security cost, described below.

## 3. Interacting with the sign-in screen is not unlocking Windows

With D the helper behaves like a physical keyboard and mouse attached to PC 2: it can move the
pointer, click, and press keys on the lock/sign-in UI. **Windows still does all authentication.**
MouseKey LAN would not implement a credential provider, verify or store passwords, or fill in
credentials. Be aware of what that means in practice:

* If *you* type your password on PC 1's keyboard while controlling PC 2's lock screen, the
  keystrokes pass through the app as encrypted scan codes (never logged or stored, as today).
  That is inherent to any remote keyboard; it is not "capture" by the app, but it does mean the
  password transits process memory on both PCs.
* Ctrl+Alt+Del (the Secure Attention Sequence) can't be sent by normal input injection. A
  service can call `SendSAS`, but only if the "Disable or enable software Secure Attention
  Sequence" policy is changed. That is a Windows security-setting change, so it would be a
  separate, explicit approval. Most home/workgroup PCs don't require it to reach the password
  box (pressing a key/click on the lock screen is enough); domain PCs often do.

## 4. Security implications of D (what you would be approving)

1. A SYSTEM process listens for (or connects to) the network and injects input on the secure
   desktop. A bug there is a privilege-escalation risk, so it must be tiny: receive-only, no UI,
   no clipboard, no files, no pairing.
2. Any **paired** PC could then drive PC 2's lock/sign-in screen and UAC prompts. Only pair PCs
   you own.
3. Key storage: the identity key is DPAPI-bound to your user account, which SYSTEM cannot read.
   Two options:
   * **Reuse identity** (export the key to a SYSTEM/Administrators-only machine store). No
     re-pairing, but any local administrator can read that key.
   * **Separate service identity** (new key held only by the service; the user app authorises
     peers to it with a one-time approval). Stronger isolation, one extra approval step.
4. Mitigations built into the plan: off by default; installed only through a UAC prompt from the
   Settings toggle; the helper accepts input **only while WTS reports the session as locked or at
   the sign-in screen** (so it refuses to handle UAC prompts inside an unlocked session);
   authenticated, encrypted sessions from already-trusted keys only; every secure-desktop
   session is logged (who/when, never keystrokes); a one-click "Turn off and remove" that
   deletes the service; no changes to any Windows security setting.

## 5. Plan

**Phase 1 (no privileges, done now)**
* Hook watchdog no longer fights the lock screen (`crates/mkl-input/src/win.rs`, with tests).
* Behaviour confirmed unchanged: lock → return control; unlock → automatic resume;
  reboot → resumes at sign-in via autostart.

**Phase 2 (remote input at the lock / sign-in screen): not being built.** MouseKey LAN stays a
per-user app. Use RustDesk (or the PC's own keyboard) to unlock a locked PC; MouseKey LAN then
reconnects and resumes on its own.

## 6. Existing components (researched)

| Component | Can it interact with the lock screen? | Can it integrate with MouseKey LAN? |
|---|---|---|
| **Mouse Without Borders** (Microsoft PowerToys) | Yes. Microsoft documents a **service mode** (run PowerToys as administrator, enable *Use Service*) for controlling elevated apps and the lock screen from another computer ([docs](https://learn.microsoft.com/windows/powertoys/mouse-without-borders)). Its setting is off by default and needs admin rights. | **No.** It has no integration API, and two programs can't both own the keyboard/mouse hooks. It is an *alternative* to MouseKey LAN, not an add-on. |
| **RustDesk** (what you use today) | Yes, it controls the real console session, including the lock screen. | Only as a separate tool. Its `--connect <ID>` flag is documented only by the community, not in RustDesk's official docs, so a one-click launcher from MouseKey LAN would be unreliable. Not built. |
| **Windows Remote Desktop (mstsc)** | Gives you a *separate* session over RDP; it does not drive the physical console's lock screen, and connecting locks the local console. | Not a fit for "one keyboard across two physical monitors". |

## 7. Recommendation

MouseKey LAN stays a per-user app: it covers everything on an unlocked PC. The requirement
"keep working on the lock screen with the seamless edge-switching experience" **cannot be met by
MouseKey LAN** under the constraints above. The supported way to get exactly that experience is
Microsoft's own Mouse Without Borders in service mode, used *instead of* MouseKey LAN (not
alongside it). Otherwise, unlock with RustDesk or the PC's own keyboard and MouseKey LAN
resumes on its own (covered by the `locked_pc_is_skipped_and_resumes_after_unlock` and
`controller_lock_returns_control_and_resumes` tests).
