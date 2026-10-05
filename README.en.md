<div align="center">

<img src="assets/logo-512.png" alt="raycat" width="200">

# raycat

**A VPN subscription for your server: gateway and proxy for the host and Docker containers**

[![CI](https://github.com/raycat-app/raycat/actions/workflows/ci.yml/badge.svg)](https://github.com/raycat-app/raycat/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/raycat-app/raycat/badge)](https://scorecard.dev/viewer/?uri=github.com/raycat-app/raycat)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

[Русский](README.md) · English

</div>

> [!IMPORTANT]
> raycat is under development; the first version has not been released yet.

## What it is

raycat takes your VPN subscription and turns it into a reliable way out to the
internet for a server:

- **the whole host** — a systemd service;
- **Docker containers** — a gateway (`network_mode: service:raycat`) or an HTTP/SOCKS
  proxy, with no settings inside the apps;
- **the local network** — a gateway for its devices.

raycat fetches the subscription the way the Happ app for Windows or Android does
(or INCY for Android), so it works with subscriptions that providers only hand out
to these apps. Traffic goes through [xray-core](https://github.com/XTLS/Xray-core),
the same core Happ uses.

## Planned for the first version

- Several subscriptions with priorities: a main one and backups.
- Automatic failover to a live node by your rules: subscription and node
  priorities, blacklists and whitelists, no needless hopping between nodes.
- Kill switch: while the VPN is down, traffic never goes out directly, DNS included.
- A command line with clear help and a TUI with the same features.
- Static binaries for x86_64, arm64 and armv7, a Docker image, one-command install.
- A separate image variant (the `-noaes` tag) for Raspberry Pi 3/4 and other CPUs
  without hardware AES, with faster encryption on such hardware.

## Installing on a server

> [!NOTE]
> The first version has not been released yet: the installer will work once releases
> exist. Until the first stable release is published, `--channel dev` installs the
> latest dev build.

You need root, `curl` or `wget`, `tar` and `sha256sum`. One command:

```sh
curl -fsSL https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh | sudo sh
```

Or download the script first and read it:

```sh
curl -fsSLO https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh
less install.sh
sudo sh install.sh --dry-run   # the plan, no changes
sudo sh install.sh
```

The installer picks the build for your CPU (x86_64, aarch64, armv7; on CPUs without
AES, such as Raspberry Pi 3/4, it takes the `-noaes` xray core), verifies the
checksum against `SHA256SUMS` and changes nothing on any mismatch. If `gh` is
installed and signed in, it also verifies the provenance of the archive
(`gh attestation verify`). Then it lays out the files:

| What | Where |
| --- | --- |
| program | `/usr/local/bin/raycat` |
| xray core | `/usr/libexec/raycat/xray` |
| settings | `/etc/raycat/config.toml` (0600, an existing file is left alone) |
| state | `/var/lib/raycat` |
| service | `/etc/systemd/system/raycat.service` |
| completions, man page, license | `/usr/local/share/…` |

The installer does not change the host's network settings: the daemon sets up
gateway mode itself, from the settings file. With systemd, the service is enabled at
boot. On the first install it is not started yet: put your subscription link into
`/etc/raycat/config.toml` (a commented example is created for you) and run
`sudo systemctl start raycat`. After that, use `sudo raycat status`, `nodes`, `tui`
and `journalctl -u raycat -f`. Without systemd the installer prints the command to
start the daemon by hand (`raycat daemon`).

- **The dev channel** (the latest build from `main`, for the adventurous): pass the
  options after `sh -s --`: `curl -fsSL … | sudo sh -s -- --channel dev`. A specific
  version: `--version 1.2.3`.
- **Updating**: run the installer again. Settings are kept; the service restarts
  only if the files changed.
- **Removing**: `sudo sh install.sh --uninstall`. Settings and state stay;
  `--uninstall --purge` deletes them too (the provider will see a new device after a
  fresh install).
- **All options**: `sh install.sh --help`.

## Performance

Everything is optional and set in the `[xray]` section of the settings file:

```toml
[xray]
memory_limit = "96MiB"      # the default
tcp_congestion = "auto"     # auto (the default), off or an algorithm name
xhttp_connections = 4       # 1 to 16; unset means whatever the provider sets
```

- `memory_limit` — a soft memory limit for xray. A limit that is too low makes the
  Go garbage collector run constantly and burns CPU under load.
- `tcp_congestion` — TCP congestion control for connections to nodes. `auto` turns
  BBR on only when the host surely allows it (the algorithm is loaded in the kernel
  and either permitted for all processes or xray has `CAP_NET_ADMIN`), otherwise it
  logs why it did not. BBR speeds up transfers on lossy links. QUIC nodes (Hysteria2,
  XHTTP over h3) are not affected: Hysteria2 already runs on BBR by default.
- `xhttp_connections` — the number of parallel XHTTP connections. It helps when a
  provider throttles a single connection; the price is more CPU load and more
  connections to the server. Unset means the provider's settings apply (xray uses 3
  connections when there are none). Nodes where the provider set `xmux` itself are
  left alone.
- For QUIC and UDP nodes (Hysteria2, XHTTP over h3, mKCP) the daemon warns once per
  start if the host's UDP buffers are below 7.5 MiB: raise `net.core.rmem_max` and
  `net.core.wmem_max` on the host.

## Contributing

Suggestions and fixes are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) (in
Russian). Report vulnerabilities privately: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).
