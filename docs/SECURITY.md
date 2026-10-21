# Security model

## Threats considered

| Threat | Mitigation |
|---|---|
| Another computer on the same Wi-Fi sends input | Every connection is authenticated with Noise XX. Input is accepted only from a **trusted** key, and only while that device holds control (`ControlEnter`). Unknown devices are rejected before any session exists. |
| Eavesdropping on keystrokes | All traffic after the handshake is encrypted and authenticated (ChaCha20-Poly1305, X25519, BLAKE2s). Keys are per device and never leave it. |
| Man-in-the-middle during pairing | Both screens show a 6-digit code derived from the handshake hash. A MITM produces two different handshakes and therefore different codes. Pairing links / QR codes also pin the expected device id, so the connection aborts if the key doesn't match. |
| Device impersonation | Device id = SHA-256(static public key). The `Hello` id must match the authenticated key, and stored trust is bound to the exact key. |
| Replay / reordering / tampering | Per-direction counter nonces; any modified, replayed or reordered frame fails authentication and the session is dropped (see `tampered_ciphertext_is_rejected` test). |
| Unwanted pairing prompts (spam) | Prompts appear only for explicit pairing requests. "Block" stops a device permanently, and pairing requests can be turned off entirely. |
| Revocation | **Forget** deletes the trust record. When the other side next connects it receives `not_trusted` and drops its trust too. **Block** additionally refuses all future connections. |
| Malformed input from a trusted peer | Coordinates are clamped to local monitors, wheel deltas are bounded, invalid key codes are refused, and monitor lists are sanitised (size limits, max 16). Frames are limited to 60 KB. |
| Key theft at rest | The private key is encrypted with Windows DPAPI (bound to the user account). |
| Credential capture | Keystrokes are never logged or stored. The app can't see the lock screen, sign-in screen, UAC prompts or Ctrl+Alt+Del (see PLATFORM.md). |

## Explicit authorization

A new computer can only become trusted when:
1. a user on PC A starts pairing with PC B (click **Pair** or enter an address), **and**
2. the user at PC B approves the prompt after comparing the code.

## Not in scope / known limitations

* A paired device is fully trusted to send input while it holds control. Only pair computers
  you own.
* The firewall rule allows the app on all network profiles so that home networks marked
  "Public" work. Untrusted networks still can't send input (see above), but they can see the
  mDNS advertisement (device name and id). Turn discovery off in Settings on untrusted
  networks.
* Builds are not code-signed yet, so Windows SmartScreen will warn on first install.

## Clipboard and file sharing

* Only paired devices with an active encrypted session can send clipboard data or files.
* Text and images are pushed on copy; files are sent only when control moves to the other PC
  (copy/paste) or when you release a drag there. Nothing is fetched by the remote side.
* Received files are written only under `Downloads\MouseKey LAN` (drops) or a temporary folder
  (clipboard files, deleted at next start). Paths from the peer are sanitised: no `..`, no
  absolute paths or drive letters, no reserved device names; existing files are never
  overwritten (`name (1).ext`).
* Size limits: 16 MB text, 128 MB image, configurable file limit (default 4 GB, max 20 000
  files). Either side can switch clipboard or file sharing off in Settings.
* Transfers use a separate low-priority queue, so input is never delayed behind file data.
