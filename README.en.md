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

## Gateway for Docker containers

The apps live in the network namespace of the raycat container (`network_mode:
service:raycat`) and reach the internet only through the tunnel. Recommended
settings, with the gateway itself locked down:

```yaml
# compose.yml
services:
  raycat:
    image: ghcr.io/raycat-app/raycat
    restart: unless-stopped
    read_only: true
    cap_drop: [ALL]
    cap_add: [NET_ADMIN]
    security_opt: ["no-new-privileges:true"]
    tmpfs: [/tmp]
    dns: [1.1.1.1]
    volumes:
      - ./config.toml:/etc/raycat/config.toml:ro
      - raycat-state:/var/lib/raycat

  app:
    image: your-app
    network_mode: service:raycat
    depends_on:
      raycat:
        condition: service_healthy
        restart: true

volumes:
  raycat-state:
```

```toml
# config.toml
[[subscription]]
name = "main"
url = "https://…"
app = "happ"
platform = "windows"

[mode]
type = "gateway"
```

- **Permissions of the settings file.** With `cap_drop: [ALL]` root in the container
  does not bypass file permissions, so a file with mode 600 owned by another host user
  cannot be read. Make it readable: `chmod 644 config.toml` (keep the directory closed
  to others) or hand it to root: `sudo chown 0:0 config.toml`, mode 600.
- **State and the API socket** live in the `/var/lib/raycat` volume, so the container
  root stays read-only. The commands work inside the container:
  `docker compose exec raycat raycat status`.
- **`dns`** is required: Docker queries its resolver from inside the container's
  namespace, where queries are intercepted; without an explicit address the gateway
  refuses to start (names would otherwise resolve around the tunnel).
- **Readiness.** The image checks `raycat health` every 10 s: the daemon answers, xray
  is running and a node is selected; the first start gets 60 s (a panel may be slow).
  With `condition: service_healthy` the apps do not start until the VPN is ready, and
  `restart: true` restarts them together with the gateway. Check by hand:
  `docker compose exec raycat raycat health` (exit code 0 or 1 and the reason).

## Gateway for the local network

raycat can be a gateway for the devices of your network: TVs, phones, consoles
that have no VPN of their own. The devices reach the internet through the host
running raycat, which intercepts their TCP and UDP and sends it into the tunnel.

**Running it.** raycat must run in the network namespace of the host itself: as a
systemd service or in Docker with `network_mode: host`. It needs the
`CAP_NET_ADMIN` capability (a service running as root, or `cap_add: [NET_ADMIN]`).

```toml
# /etc/raycat/config.toml
[mode]
type = "gateway"
lan = true
# lan_interface = "eth0"                # default: the interface of the default route
# lan_subnets = ["192.168.1.0/24"]      # default: the subnets of that interface
```

```yaml
# compose.yml
services:
  raycat:
    image: ghcr.io/raycat-app/raycat
    network_mode: host
    cap_add: [NET_ADMIN]
    restart: unless-stopped
    environment:
      RAYCAT_SUBSCRIPTION: https://…
      RAYCAT_APP: happ
      RAYCAT_PLATFORM: windows
      RAYCAT_MODE: gateway
      RAYCAT_LAN: "true"
```

`raycat check` shows which interface and subnets will be intercepted.

**Devices.** Set the host address as both the gateway and the DNS server: on the
device itself, or in the DHCP of the router that hands out addresses. Device DNS
(any server, port 53) goes through raycat too, so names resolve to placeholder
addresses (fake-IP), just like on the host itself. Device IPv6 is blocked: turn it
off on the device or in the router, or the device will bypass the host.

**The host needs nothing else.** Do not enable forwarding (`ip_forward`): intercepted
packets are delivered locally, and raycat does not touch sysctl. If forwarding is on
for other reasons, the kill switch still keeps the devices from leaving directly,
bypassing the tunnel.

**The host stays reachable.** Incoming connections to the host (SSH and other
services) and the replies to them are left alone at start, when xray fails and at
stop. The host's own addresses, private networks (Docker networks and the other
devices of your network included), multicast and broadcast are not intercepted
either. On a clean stop raycat removes its rules; after a crash they stay in place to
hold the kill switch, and the next start installs them again.

## Versions and channels

Releases are automatic, each channel has its own tags.

| Channel | What it is | Docker image | Files |
| --- | --- | --- | --- |
| **stable** | Production versions `vX.Y.Z`. They went through dev and waited out a hold period: one day for emulation profile changes, three days for everything else. Breaking changes are released by hand only | `ghcr.io/raycat-app/raycat:latest`, `:X.Y.Z`, `:X.Y` | [Releases](https://github.com/raycat-app/raycat/releases/latest) |
| **dev** | A build of every merge into `main` after green CI, `vX.Y.Z-dev.N`. For early testing, not for production servers; the last 20 are kept | `:dev`, `:X.Y.Z-dev.N` | a pre-release on the Releases page |

Images are multi-architecture (amd64 and arm64). For **Raspberry Pi 3/4 and other
processors without hardware AES** (no `aes` flag in `/proc/cpuinfo`) there are variants
with xray built for fast AES-GCM on such hardware: the images `:latest-noaes`,
`:X.Y.Z-noaes`, `:dev-noaes` (arm64 only) and the archives with `-noaes` in the name
(aarch64 and armv7).

While an open issue carries the `стоп-релиз` ("stop release") label, promotion from dev
to stable is paused.

### Verifying authenticity

Archives and images carry build provenance attestations (GitHub); images also carry an
SBOM. Verification needs [GitHub CLI](https://cli.github.com/):

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-X.Y.Z-x86_64-linux-musl.tar.gz --repo raycat-app/raycat
gh attestation verify oci://ghcr.io/raycat-app/raycat:latest --repo raycat-app/raycat
```

## Contributing

Suggestions and fixes are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) (in
Russian). Report vulnerabilities privately: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).
