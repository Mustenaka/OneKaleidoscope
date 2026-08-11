# T-110 real-gate evidence

Status: **pending — not executed in this implementation workspace**

This file intentionally contains no loopback, emulator, mock, public-relay, or
`adb reverse` result presented as acceptance. Local and CI evidence belongs in
the pull request/check runs. T-110 closes only after the table below is filled
from the same committed candidate on an owned Ubuntu instance, a home PC, and
a physical arm64 Android device using cellular data.

| Field | Required evidence | Current result |
|---|---|---|
| Candidate | full 40-character commit; clean checkout | pending |
| Ubuntu | distribution/kernel, `kaleido-relay` build revision, self-hosted DNS and open-port summary | pending |
| Android | model-independent OS/API/ABI, app revision; no serial/advertising ID | pending |
| PC | OS/version and hostd revision; no username/path | pending |
| Topology | home PC outbound, phone cellular with Wi-Fi/VPN off, owned Ubuntu only | pending |
| Initial path | authenticated `PeerToPeer` or `Relayed`, with elapsed time | pending |
| Forced P2P failure | bounded transition to authenticated `Relayed` | pending |
| R3 projections | all seven real Codex projection classes remain visible | pending |
| Mobile ingress | one real prompt/queued input and one structured Attention result | pending |
| FCM | real data-only FID delivery wakes background WorkManager | pending |
| Handover/cold start | cellular↔Wi-Fi plus process kill restores exact per-key cursors | pending |
| PC outage | timed `Offline`, then recovery without command replay | pending |
| Revoke | LAN, P2P, relay and later push all reject the revoked device | pending |
| Confidentiality | relay logs, packet capture and in-process diagnostics have zero business-canary hits | pending |

Before recording anything, run from the clean candidate checkout on the PC:

```powershell
./scripts/t110-physical-preflight.ps1
```

Retain the JSON output with the private test record. It rejects emulators,
non-arm64 devices, enabled Wi-Fi, VPN transport, `adb reverse`, dirty source,
and an ambiguous device set. It is only a preflight, not acceptance evidence.

For every path transition record only the enum (`PeerToPeer`, `Relayed`, or
`Offline`), monotonic elapsed time, and redacted cursor tuple. Do not record
public credentials, complete endpoints, FID, DeviceId, payload/content,
provider arguments, usernames, or filesystem paths in this file.
