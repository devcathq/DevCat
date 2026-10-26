# Running your own relay

DevCat connects devices directly whenever the networks allow it (same Wi-Fi, hole-punching, or the
router opening DevCat's port through UPnP / NAT-PMP / PCP). When they don't (some mobile networks,
strict office firewalls), the traffic goes through a **relay**. The relay only forwards packets that
are end-to-end encrypted between the two devices; it can't read them.

By default DevCat uses n0's public relays. They're shared and rate-limited, and may be far from you.
A relay of your own, close to your users, makes those connections much faster.

DevCat → Mobile shows each phone's connection as **direct** or **through relay …** with its
round-trip time, so you can see how often the relay is actually used.

## What you need

- A small Linux server (1 vCPU, 1 GB RAM is plenty), close to your users.
- A DNS name pointing at it, e.g. `relay-sgp.example.com`.
- Open ports: TCP 80 and 443, UDP 7842.

## Install

```sh
cargo install iroh-relay --version 1.3.0 --features server
```

## Configure (`/etc/iroh-relay.toml`)

```toml
enable_relay = true
# Lets devices learn their public address over QUIC (helps hole-punching).
enable_quic_addr_discovery = true

[tls]
cert_mode = "LetsEncrypt"
hostname = "relay-sgp.example.com"
contact = "you@example.com"
cert_dir = "/var/lib/iroh-relay/certs"
prod_tls = true
```

The defaults bind HTTP on `[::]:80` (needed for Let's Encrypt), HTTPS on `[::]:443` and QUIC on
`[::]:7842`.

Optional: `access.allowlist = ["<endpoint id>", …]` only relays for the listed DevCat installs
(their iroh endpoint ids). Leaving it open (the default) is safe for privacy, since everything is
encrypted end to end; it only costs you bandwidth.

## Run

```sh
iroh-relay --config-path /etc/iroh-relay.toml
```

As a systemd service:

```ini
[Unit]
Description=iroh relay for DevCat
After=network-online.target

[Service]
ExecStart=/root/.cargo/bin/iroh-relay --config-path /etc/iroh-relay.toml
Restart=always

[Install]
WantedBy=multi-user.target
```

## Use it in DevCat

DevCat → Mobile → **Reach from other networks** → **My own relay**, and enter
`https://relay-sgp.example.com`. Phones that paired before pick it up the next time they connect;
the pairing QR carries it from then on.
