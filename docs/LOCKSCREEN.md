# Lock screen support

Status: implemented in 0.3.4 as an optional, off-by-default **lock-screen service**
(`crates/mkl-service`). Without it, MouseKey LAN behaves as before: a locked PC is skipped and
sharing resumes when it is unlocked with its own keyboard.

## Why a service is needed

When Windows locks, the password screen (LogonUI) runs on the **Winlogon secure desktop**.
Windows only accepts input there from SYSTEM processes attached to that desktop. MouseKey LAN
is a per-user app (optionally elevated with *Run as administrator*), so `SendInput` fails with
`ERROR_ACCESS_DENIED` (error 5) as soon as the password screen appears. The lock-screen picture
shown first still accepts input, which is why the mouse used to work for a few seconds and
then stop.

Mouse Without Borders (service mode), Synergy/Deskflow and RustDesk solve this the same way: a
LocalSystem service runs a helper on the user's session that injects on the secure desktop.

An earlier attempt (a `uiAccess` helper started by the "MouseKey LAN Lock Helper" scheduled
task, and a service entry for a program that wasn't a service) could not work: `uiAccess`
doesn't grant access to the secure desktop, and the app never sent it any input. Installing or
upgrading the service removes that task.

## How it works

```
controlling PC ──(Noise session, unchanged)──► MouseKey LAN app on this PC
                                                  │ SendInput
                                                  ├─ works: done (unlocked, or the lock picture)
                                                  └─ ERROR_ACCESS_DENIED (password screen)
                                                        │ \\.\pipe\MouseKeyLAN.SecureInput
                                                        ▼
                     mkl-service --agent (SYSTEM, user's session) ── SendInput on Winlogon
                        ▲ started and watched by
                     mkl-service --service (LocalSystem Windows service, auto-start)
```

* **Service** (`service.rs`): keeps exactly one agent in the active console session. It starts
  the agent with a copy of its own token moved to that session (`DuplicateTokenEx`,
  `SetTokenInformation(TokenSessionId)`, `CreateProcessAsUserW`), restarts it with back-off if
  it exits, and moves it when the console session changes.
* **Agent** (`agent.rs`): serves one pipe connection at a time. Each 12-byte record
  (`mkl_input::secure::encode`) is decoded strictly, then injected after attaching the thread
  to the input desktop (`OpenInputDesktop`, `SetThreadDesktop`). It answers one byte per event.
* **App** (`mkl_input::secure`, `WinInjector::inject`): forwards an event to the agent only when
  its own `SendInput` fails with `ERROR_ACCESS_DENIED`. While locked it tells peers it can take
  input (`Status{locked:false}`) only if the agent is reachable, re-checking every 2 s because
  the service may start after the lock. With the agent, the PC driving this one keeps control
  through the lock so it can type the password.
* **Started already locked:** Windows can sign in and lock by itself after a restart. The app
  then never receives a lock notification, so the hook thread checks the session's lock state
  at start (`WTSQuerySessionInformationW`, `WTSSessionInfoEx`).

## Security

* **The agent only injects while the session is locked and the secure desktop has the input.**
  Everywhere else it refuses, so it can't be used to click UAC prompts in an unlocked session.
* **The pipe admits only SYSTEM and elevated administrators** (`D:P(A;;GA;;;SY)(A;;GA;;;BA)`),
  rejects remote clients, and is created as the first instance, so the agent never joins a pipe
  someone else created. An elevated administrator can already do everything the agent can, so
  this grants no new rights. It also means the app needs *Run as administrator*.
* **The app talks only to a pipe served by SYSTEM.** It checks the server process's token
  (`GetNamedPipeServerProcessId`, `TokenUser` is LocalSystem) and connects with
  `SECURITY_IDENTIFICATION`, so another program can't pose as the agent and read keystrokes,
  or act as the app.
* **No network, files, clipboard or credentials in SYSTEM code.** The agent only receives input
  events from the already-authenticated app. Networking, pairing and keys stay in the app.
* The service runs only from Program Files (admin-only), with a quoted path, restricted to the
  privileges it needs to start the agent (`SeTcb`, `SeAssignPrimaryToken`, `SeIncreaseQuota`).
* Logs (`<install dir>\logs\service.log`, `agent.log`) record starts, stops, connections and
  counts, never input.
* **What enabling it means:** any PC paired with this one can use its lock screen, including
  typing the password. Only pair PCs you own. Password keystrokes travel encrypted from the
  controlling PC, like any remote keyboard input, and are never logged or stored.

## Limits

* Works while a user is signed in and the PC is locked (including Windows' automatic sign-in
  and lock after a restart). Not the sign-in screen after signing out: the app isn't running.
* Ctrl+Alt+Del (the Secure Attention Sequence) can't be sent. Most home and workgroup PCs don't
  ask for it on the lock screen.
* The controlled PC needs the service installed and *Run as administrator* turned on.

## Install, remove, logs

* **Settings → Security → Lock-screen control → Install service** (one UAC prompt) runs
  `mkl-service.exe --install`; **Uninstall** runs `--uninstall`.
* The installer stops the service before replacing files, runs `--upgrade` afterwards (repairs
  and restarts the service only if it was installed), and runs `--uninstall` on uninstall.
* Check it: `sc query MouseKeyLANService` shows `RUNNING`, and Settings shows **ready**.
