<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
  <img src="assets/logo.png" alt="raycat" width="180">
</picture>

# raycat

A VPN subscription for your Linux server: containers, the whole server or your home network go online through it

[![Version](https://img.shields.io/github/v/release/raycat-app/raycat?include_prereleases&style=flat-square&label=version&color=2d333b)](https://github.com/raycat-app/raycat/releases)
[![License](https://img.shields.io/badge/license-MIT-2d333b?style=flat-square)](LICENSE)

[Русский](README.md) · English

</div>

<p align="center">
  <img src="assets/tui.png" alt="raycat tui: overview" width="820">
</p>

You give raycat a subscription link, and it keeps a tunnel running on the server. You don't have to configure the applications themselves. If a node dies, raycat switches to a live one. If none are left, traffic does not leave outside the VPN.

Many providers only hand their subscriptions to their own apps. raycat requests them the way Happ (Windows and Android) or INCY (Android) does, and builds the tunnel on [xray-core](https://github.com/XTLS/Xray-core).

> [!NOTE]
> There is no stable version yet. Until it ships, use the `ghcr.io/raycat-app/raycat:dev` image and install with `--channel dev`.

## Getting started

### In Docker

The `app` container here is your application: all its traffic goes through the VPN.

`raycat/config.toml`:

```toml
[[subscription]]
url = "https://…"       # the subscription link, keep it private
app = "happ"            # the app your provider accepts: happ or incy
platform = "windows"    # windows or android (incy is android only)

[mode]
type = "gateway"
```

`compose.yml` next to the `raycat` directory:

```yaml
services:
  raycat:
    image: ghcr.io/raycat-app/raycat
    restart: unless-stopped
    cap_add: [NET_ADMIN]
    dns: [1.1.1.1]
    volumes:
      - ./raycat:/etc/raycat
      - raycat-state:/var/lib/raycat

  app:
    image: your-application
    network_mode: service:raycat
    depends_on:
      raycat:
        condition: service_healthy
        restart: true

volumes:
  raycat-state:
```

```sh
docker compose up -d
docker compose exec raycat raycat tui
```

### On a server

```sh
curl -fsSL https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh | sudo sh
sudo raycat init --force    # asks for the link, the app and the mode
sudo systemctl start raycat
sudo raycat tui
```

`proxy` mode leaves the rest of the server's traffic alone and opens an HTTP and SOCKS5 proxy on `127.0.0.1:7890`. `gateway` mode sends the whole server through the VPN.

## What it does

- **Several subscriptions.** A main one and backups, with configurable order and priority.
- **Node selection.** Checks nodes itself and switches on failure. You can pin a node or exclude nodes with masks like `Bypass*`.
- **Gateway for your home network.** A TV or console goes online through the server once you set it as the gateway.
- **Russia direct.** With `ru_direct = true`, Russian sites bypass the VPN.
- **Kill switch.** In gateway mode, nothing leaves without the tunnel, DNS included.
- **Lightweight.** Static builds for x86_64, arm64 and armv7. It runs even on a Raspberry Pi.

## Managing it

`raycat tui` shows everything on one screen and works with a mouse: status, nodes, subscriptions, the log and settings. You can change settings right there.

<p align="center">
  <img src="assets/tui-nodes.png" alt="raycat tui: nodes" width="820">
</p>

The same things are available as commands:

| Command | What it does |
| --- | --- |
| `raycat status` | tunnel state, current node, subscriptions |
| `raycat nodes` | list of nodes |
| `raycat use <node>`, `raycat use auto` | pin a node, return to automatic selection |
| `raycat update` | update subscriptions now |
| `raycat speedtest` | test the speed through the VPN |
| `raycat check` | check the settings file |
| `raycat config` | show the effective settings, with secrets hidden |
| `raycat reload` | apply edits to the settings file |

`raycat --help` shows the rest.

## Settings

Settings live in one file: `/etc/raycat/config.toml` on a server, `raycat/config.toml` in the Docker example. Every key is explained in the [example settings](deploy/config.example.toml), and raycat suggests the right name if you make a typo.

After editing, run `sudo raycat reload`. The daemon applies what it can on the fly and tells you if something needs a restart.

<details>
<summary>Home network</summary>

raycat must run in the host's own network: as a service, or in Docker with `network_mode: host`.

```toml
[mode]
type = "gateway"
lan = true
```

On the device or in your router's DHCP, set the server's address as the gateway and DNS. You don't need to enable `ip_forward`, and SSH and other host services keep working. `raycat check` shows which interface and subnets will be intercepted.

</details>

<details>
<summary>Environment variables</summary>

These override the settings file, which is handy in Docker.

| Variable | What it sets |
| --- | --- |
| `RAYCAT_SUBSCRIPTION`, `RAYCAT_SUBSCRIPTION_FILE` | the link of the first subscription, or a file with it |
| `RAYCAT_APP`, `RAYCAT_PLATFORM` | the app and platform of the first subscription |
| `RAYCAT_MODE`, `RAYCAT_KILL_SWITCH`, `RAYCAT_LAN`, `RAYCAT_LISTEN` | the operating mode |
| `RAYCAT_PROXY_AUTH`, `RAYCAT_PROXY_AUTH_FILE` | the proxy login and password |
| `RAYCAT_SEED`, `RAYCAT_LOG` | the device and the log level |
| `RAYCAT_CONFIG`, `RAYCAT_STATE_DIR`, `RAYCAT_SOCKET` | paths |

</details>

<details>
<summary>Troubleshooting</summary>

**The provider returns no nodes.** Run `raycat fetch <subscription>` to see the provider's response. Errors 403 and 404 usually mean you need a different app or platform. The device limit may also be used up, or the subscription may have expired.

**`raycat status` says the daemon is not running.** Check `sudo systemctl status raycat` and `journalctl -u raycat -n 100`. Client commands need `sudo`.

**The gateway doesn't start in Docker.** It needs `cap_add: [NET_ADMIN]` and `dns: [1.1.1.1]`, as in the example.

**Settings from the TUI don't save in Docker.** Mount the directory, not a single file, as in the example.

**Slow on a Raspberry Pi 3 or 4.** These processors have no hardware AES. The installer picks the right build by itself. In Docker, add `-noaes` to the tag, for example `:latest-noaes`.

</details>

<details>
<summary>Updates and verifying builds</summary>

There are two channels. `dev` is built from every change. `stable` ships by itself once a build has spent enough time on dev. xray updates and vulnerability fixes ship immediately. To update a server install, run the install command again, and your settings stay. To remove it: `sudo sh install.sh --uninstall`.

Archives and images are signed with GitHub attestations:

```sh
gh attestation verify raycat-X.Y.Z-x86_64-linux-musl.tar.gz --repo raycat-app/raycat
gh attestation verify oci://ghcr.io/raycat-app/raycat:latest --repo raycat-app/raycat
```

</details>

## Questions and bugs

Ask questions in [Discussions](https://github.com/raycat-app/raycat/discussions) and report bugs in [issues](https://github.com/raycat-app/raycat/issues). Never post your subscription link or the output of `raycat identity`. Report vulnerabilities [privately](https://github.com/raycat-app/raycat/security/advisories/new).

Licensed under [MIT](LICENSE).
