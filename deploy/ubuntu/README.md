# Ubuntu relay deployment

This directory deploys the production `kaleido-relay` binary. It does not
provision paid infrastructure, DNS, Firebase credentials, or claim the T-110
physical-device gate.

Build the exact workspace revision on Ubuntu:

```bash
cargo build --locked --release -p kaleido-relay --all-features
sudo install -D -o root -g root -m 0755 \
  target/release/kaleido-relay \
  /usr/local/lib/onekaleidoscope/kaleido-relay
```

Create the unprivileged account and private storage before installing secrets:

```bash
sudo useradd --system --home /var/lib/onekaleidoscope-relay \
  --shell /usr/sbin/nologin onekaleidoscope-relay
sudo install -d -o onekaleidoscope-relay -g onekaleidoscope-relay -m 0700 \
  /var/lib/onekaleidoscope-relay/security
sudo install -d -o root -g onekaleidoscope-relay -m 0750 /etc/onekaleidoscope
sudo install -o root -g onekaleidoscope-relay -m 0640 \
  deploy/ubuntu/relay.env.example /etc/onekaleidoscope/relay.env
sudo install -o onekaleidoscope-relay -g onekaleidoscope-relay -m 0600 \
  /secure/operator/path/fcm-adc.json \
  /var/lib/onekaleidoscope-relay/security/fcm-adc.json
```

Replace every `.invalid`/placeholder in `relay.env`. The relay hostname needs
public A/AAAA DNS. Allow inbound TCP 80/443 for ACME and relay, UDP 7842 for
QUIC, and TCP 7443 for the pinned control plane. Keep 8787 loopback-only.

Install and start the hardened unit:

```bash
sudo install -o root -g root -m 0644 \
  deploy/ubuntu/onekaleidoscope-relay.service \
  /etc/systemd/system/onekaleidoscope-relay.service
sudo systemctl daemon-reload
sudo systemctl enable --now onekaleidoscope-relay.service
curl --fail --silent http://127.0.0.1:8787/healthz
```

Export the control SPKI pin only through the operator channel; do not put it,
route credentials, endpoint IDs, or FIDs in service logs:

```bash
sudo -u onekaleidoscope-relay \
  env $(sudo sed '/^#/d' /etc/onekaleidoscope/relay.env | xargs) \
  /usr/local/lib/onekaleidoscope/kaleido-relay --print-service-pin
```

For rolling upgrades, replace the binary atomically, run `systemctl restart`,
and verify `/healthz`. Host presence deliberately expires after at most 90
seconds and must be refreshed after a service restart. Registry or identity
corruption and broadened Unix permissions fail startup; never delete them to
make startup green.
