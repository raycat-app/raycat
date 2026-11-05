<div align="center">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
  <img src="assets/logo.svg" alt="raycat" width="200">
</picture>

# raycat

**A VPN subscription for your Linux server.**<br>
Chosen containers, the whole server or your home network go online through it.

[![Version](https://img.shields.io/github/v/release/raycat-app/raycat?include_prereleases&style=flat-square&label=version&color=2d333b)](https://github.com/raycat-app/raycat/releases)
[![License](https://img.shields.io/badge/license-MIT-2d333b?style=flat-square)](LICENSE)

[Getting started](#getting-started) · [Modes](#modes) · [Managing it](#managing-it) · [Settings](#settings) · [Questions](#questions-and-bugs) · [Русский](README.md)

<br>

<img src="assets/tui.png" alt="raycat tui: overview" width="820">

</div>

<br>

You give raycat a subscription link, and it keeps a tunnel running on the server. You don't have to configure the applications themselves. If a node dies, raycat switches to a live one. If none are left, traffic does not leave outside the VPN.

Many providers only hand their subscriptions to their own apps. raycat requests them the way Happ (Windows and Android) or INCY (Android) does, and builds the tunnel on [xray-core](https://github.com/XTLS/Xray-core).

> [!NOTE]
> There is no stable version yet. Until it ships, use the `ghcr.io/raycat-app/raycat:dev` image and install with `--channel dev`.

## Getting started

### In Docker

Only the containers you attach to raycat go through the VPN. The rest of the server and other containers work as usual.

`raycat/config.toml`:

```toml
[[subscription]]
url = "https://…"       # the subscription link
app = "happ"            # the app your provider accepts: happ or incy
platform = "windows"    # windows or android (incy is android only)

[mode]
type = "gateway"
```

> [!CAUTION]
> The subscription link works like a password. Don't publish it or keep it in public repositories.

`compose.yml` next to the `raycat` directory. The `app` container here is your application:

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
    network_mode: service:raycat   # this line sends the container through the VPN
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

## Modes

| What goes through the VPN | How to run it | Setting |
| --- | --- | --- |
| **Chosen containers** | raycat in Docker, containers with `network_mode: service:raycat` | `type = "gateway"` |
| **The whole server** | raycat as a service on the server | `type = "gateway"` |
| **Chosen programs** | an HTTP and SOCKS5 proxy on `127.0.0.1:7890` | `type = "proxy"` |
| **Your home network** | raycat in the host network, the server set as the gateway on devices | `type = "gateway"`, `lan = true` |

> [!IMPORTANT]
> The kill switch works in `gateway` mode: without the tunnel, traffic and DNS do not leave. In `proxy` mode, programs that don't use the proxy go direct.

**raycat can also:**

- keep a main subscription and backups and reorder them on the fly;
- check nodes and switch on failure, pin a node or exclude nodes with masks like `Bypass*`;
- send Russian sites around the VPN (`ru_direct = true`);
- run wherever your server does: builds for x86_64, arm64 and armv7, including Raspberry Pi.

## Managing it

`raycat tui` shows everything on one screen and works with a mouse. You can change settings right there.

<div align="center">
  <img src="assets/tui-nodes.png" alt="raycat tui: nodes" width="820">
</div>

<br>

| Key | Tab | What you can do there |
| --- | --- | --- |
| <kbd>1</kbd> | Overview | see the tunnel state, update subscriptions, test the speed |
| <kbd>2</kbd> | Nodes | pin a node <kbd>p</kbd>, exclude it <kbd>x</kbd>, hide dead ones <kbd>d</kbd>, test the speed <kbd>t</kbd> |
| <kbd>3</kbd> | Subscriptions | change masks <kbd>m</kbd> and the order <kbd>[</kbd> <kbd>]</kbd> |
| <kbd>4</kbd> | Log | see events, warnings only <kbd>w</kbd> |
| <kbd>5</kbd> | Settings | change the main settings |

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

> [!TIP]
> After editing, run `sudo raycat reload`. The daemon applies what it can on the fly and tells you if something needs a restart.

<details>
<summary><b>Your own rules: what goes around the VPN</b></summary>

<br>

Normally all traffic goes through the VPN. Rules are for exceptions. For example, open your bank's site directly, and send one Russian site through the VPN instead:

```toml
[routing]
ru_direct = true

[[routing.rule]]
domains = ["mybank.example"]
action = "direct"             # direct: around the VPN

[[routing.rule]]
domains = ["*.example.ru"]    # *.example.ru: the site and its subdomains
action = "proxy"              # proxy: through the VPN, block: block it
```

Rules are checked from top to bottom, and the first match wins. They take priority over `ru_direct`. Instead of `domains`, or together with them, you can set `ips`: subnets or single addresses.

</details>

<details>
<summary><b>Home network</b></summary>

<br>

raycat must run in the host's own network: as a service, or in Docker with `network_mode: host`.

```toml
[mode]
type = "gateway"
lan = true
```

On the device or in your router's DHCP, set the server's address as the gateway and DNS. You don't need to enable `ip_forward`, and SSH and other host services keep working. `raycat check` shows which interface and subnets will be intercepted.

</details>

<details>
<summary><b>Environment variables</b></summary>

<br>

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
<summary><b>Troubleshooting</b></summary>

<br>

**The provider returns no nodes.** Run `raycat fetch <subscription>` to see the provider's response. Errors 403 and 404 usually mean you need a different app or platform. The device limit may also be used up, or the subscription may have expired.

**`raycat status` says the daemon is not running.** Check `sudo systemctl status raycat` and `journalctl -u raycat -n 100`. Client commands need `sudo`.

**The gateway doesn't start in Docker.** It needs `cap_add: [NET_ADMIN]` and `dns: [1.1.1.1]`, as in the example.

**Settings from the TUI don't save in Docker.** Mount the directory, not a single file, as in the example.

**Slow on a Raspberry Pi 3 or 4.** These processors have no hardware AES. The installer picks the right build by itself. In Docker, add `-noaes` to the tag, for example `:latest-noaes`.

</details>

<details>
<summary><b>Updates and verifying builds</b></summary>

<br>

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
