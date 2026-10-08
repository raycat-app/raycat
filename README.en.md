<div align="center">

<img src="assets/logo-512.png" alt="raycat" width="200">

# raycat

**A VPN subscription for your server: gateway and proxy for the host and Docker containers**

[![CI](https://github.com/raycat-app/raycat/actions/workflows/ci.yml/badge.svg)](https://github.com/raycat-app/raycat/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/raycat-app/raycat/badge)](https://scorecard.dev/viewer/?uri=github.com/raycat-app/raycat)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![version](https://img.shields.io/github/v/release/raycat-app/raycat?include_prereleases&label=version)](https://github.com/raycat-app/raycat/releases)

[Русский](README.md) · English

</div>

> [!IMPORTANT]
> raycat is under development; the first version has not been released yet.

<p align="center">
  <img src="assets/tui.svg" alt="raycat tui" width="100%">
</p>

## What it is

raycat is a VPN subscription client for Linux servers. It fetches the subscription from your
provider and routes the traffic of the whole server, of Docker containers or of devices on
your home network through the VPN, without any settings in each application. Subscriptions
that providers issue only to the Happ and INCY apps work too: raycat requests them the same
way those apps do. The tunnel is built by [xray-core](https://github.com/XTLS/Xray-core), the
same core that runs inside Happ.

## Features

- **Apps in Docker reach the internet only through the VPN, with no settings inside them.**
  A container connects to the raycat gateway, and all of its traffic goes through the tunnel.
- **The whole server or individual programs.** Host traffic can be routed through the VPN
  entirely (a systemd service), or only for programs that use the proxy `127.0.0.1:7890`
  (HTTP and SOCKS5).
- **Home-network devices without their own VPN.** A TV, a phone or a set-top box reaches the
  internet through the server running raycat.
- **Subscriptions that ordinary clients cannot open.** raycat fetches the subscription the same
  way Happ for Windows and Android or INCY for Android does.
- **Backup subscriptions and switching to a live node.** Priorities for subscriptions and
  nodes, allow and deny lists of nodes by name masks, switching without needless hops between
  nodes.
- **Kill switch (an emergency cutoff: without the tunnel, traffic does not leave outside the VPN).**
  While the tunnel is down, the traffic of devices and containers does not go out directly,
  DNS included.
- **Control without restart.** The commands `raycat status`, `nodes`, `use`, `update` and the
  full-screen `raycat tui`: pin a node, update subscriptions, watch events.
- **Lightweight and portable.** Static binaries for x86_64, arm64 and armv7 (Raspberry Pi), a
  Docker image, installation with one command. The memory limit of xray is configurable
  (96 MiB by default). For Raspberry Pi 3/4 and other CPUs without hardware AES there is a
  variant with the `-noaes` tag.

## Quick start

Before the first run, install raycat: [installing on a server](#installing-on-a-server).

### Docker: gateway for containers

```yaml
# compose.yml
services:
  raycat:
    image: ghcr.io/raycat-app/raycat
    restart: unless-stopped
    cap_add: [NET_ADMIN]
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

Stronger isolation of the gateway and other options are described in the
[Docker section](#gateway-for-docker-containers).

### Whole server (systemd)

The installer creates `/etc/raycat/config.toml` with an example. The setup wizard replaces it
with your settings and then the service is started:

```sh
sudo raycat init --force
sudo systemctl start raycat
sudo raycat status
```

`raycat init` asks for the subscription link (it is not shown on the screen), the app, the
platform and the mode: a proxy or a gateway for the whole server. The old file is kept as
`config.toml.bak`. Without a terminal, for example in a script, pass the values as flags:
`sudo raycat init --subscription https://… --mode gateway` (help: `raycat init --help`).

### Proxy for apps

In `proxy` mode (the default) raycat listens on `127.0.0.1:7890` and accepts HTTP and SOCKS5.
No password is needed on this server. To let others in, set a login and a password (see below).
Set up the subscription as in the example above, but without the `[mode]` block, and start the
service (`sudo systemctl start raycat`) or `raycat daemon`. Then point the program at the proxy:

```sh
curl -x socks5h://127.0.0.1:7890 https://example.com
export HTTPS_PROXY=http://127.0.0.1:7890   # for programs that read environment variables
```

#### Proxy with a password

To let others (containers, computers on the network) use the proxy, set a login and a password
in `[mode]`:

```toml
[mode]
listen = "0.0.0.0:7890"
auth_file = "/run/secrets/raycat_proxy"   # or auth = "login:password"
```

The file holds one line `login:password`; spaces and line breaks at the edges do not matter, and
the size is at most 1 KiB. The login is up to 128 characters, the password 8 to 128 characters,
with no control characters. Set only one of the keys: `auth` or `auth_file`. Variables:
`RAYCAT_PROXY_AUTH`, `RAYCAT_PROXY_AUTH_FILE`. The password works for both HTTP and SOCKS5:

```sh
curl -x http://login:password@192.168.1.10:7890 https://example.com
curl -x socks5h://login:password@192.168.1.10:7890 https://example.com
```

Docker example: raycat serves the proxy to other containers of the compose network. The password
lies in a secret, and both the gateway and the app read it. The permissions of the secret file are
the same as for `config.toml` in the container (see "Permissions of the settings file" in the Docker section):

```yaml
# compose.yml
services:
  raycat:
    image: ghcr.io/raycat-app/raycat
    restart: unless-stopped
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    volumes:
      - ./config.toml:/etc/raycat/config.toml:ro
      - raycat-state:/var/lib/raycat
    secrets: [raycat_proxy]

  app:
    image: your-app
    secrets: [raycat_proxy]
    depends_on: [raycat]
    # Proxy: http://raycat:7890 with the login and the password from the secret, for example:
    # sh -c 'export HTTPS_PROXY="http://$(cat /run/secrets/raycat_proxy)@raycat:7890"; exec your-program'

volumes:
  raycat-state:

secrets:
  raycat_proxy:
    file: ./raycat_proxy.txt    # one line: login:password
```

In `config.toml` for this example, set `listen` and `auth_file` in the `[mode]` block, as above.

## How it works

```mermaid
flowchart LR
    sub["Provider subscription"] -- "requested the way Happ or INCY requests it" --> rc["raycat"]
    rc -- "picks the node, keeps the kill switch" --> xr["xray"]
    ct["Docker containers<br/>(gateway mode)"] -- "traffic interception" --> xr
    lan["LAN devices<br/>(lan = true)"] -- "traffic interception" --> xr
    app["Host programs<br/>(proxy 127.0.0.1:7890)"] -- "HTTP and SOCKS5" --> xr
    xr --> node["VPN node"] --> net["Internet"]
```

raycat fetches the subscription, picks a node by priorities and rules, monitors it and switches
when it fails. xray brings up the tunnel and carries the traffic. In gateway mode raycat sets up
nftables and routing rules, so the traffic of the host, the containers and the devices is
intercepted transparently (TPROXY, a Linux kernel mechanism in which applications know nothing
about the proxy) and sent to xray. In proxy mode there are no rules: programs connect to the
proxy themselves.

## Commands

Client commands (`status`, `health`, `nodes`, `use`, `update`, `events`, `tui`) talk to the
running service through a socket and do not read the settings file. Help: `raycat --help` and
`raycat help <command>`.

| Command | What it does |
| --- | --- |
| `raycat init [--force]` | Setup wizard: asks for the subscription link, the app, the platform and the mode, and writes `/etc/raycat/config.toml` (or `--config`). Without a terminal, use the flags, see `raycat init --help`. |
| `raycat daemon` | Runs in the foreground: fetches subscriptions and keeps xray running. This is how the service and the container start. |
| `raycat check` | Validates the settings, builds the xray config from the subscription cache and runs `xray run -test`. The cache is needed: the daemon must have fetched a subscription. |
| `raycat fetch <subscription>` | Fetches a subscription once and shows the response, the provider's information and the nodes. Applies and saves nothing. |
| `raycat identity` | Shows the device emulated for each subscription. The HWID is printed in full: do not publish this output. |
| `raycat status [--json]` | Daemon state: mode, xray, current node, subscriptions. |
| `raycat health` | A quick readiness check: the daemon responds, xray runs, a node is selected. Exit code 0 or 1, for HEALTHCHECK. |
| `raycat nodes [-s subscription] [--all] [--json]` | Node table. By default the live nodes and the selected one; `--all` shows all of them. |
| `raycat use <node>` | Pin a node: its name, a part of its name (case-insensitive) or `subscription/name`. |
| `raycat use auto` | Remove the pin and return to automatic selection. |
| `raycat update [subscription]` | Update subscriptions now (all or one) and show the result. |
| `raycat events [--json]` | Follow the daemon's events until Ctrl+C. |
| `raycat tui` | Full-screen interface (see below). |
| `raycat completions bash\|zsh\|fish` | Shell completion script. |

The `--config PATH` option is available for `daemon`, `check`, `fetch` and `identity`. Exit codes:
0 means success, 1 an error, 2 an error in the arguments.

### TUI

`raycat tui` shows the state in real time: the header (mode, whether xray runs, the kill switch,
the current node and the reason it was chosen), the "Subscriptions", "Nodes" and "Log" sections
(the last 200 events) and a hint line at the bottom. The `▶` marker is on the selected node, and
`★` is on the node pinned by hand. The TUI requires a terminal and a window of at least 44×12. If
the daemon is unavailable, the screen shows the reason and reconnects by itself.

| Key | Action |
| --- | --- |
| `↑` `↓`, `j` `k` | select a node |
| `PgUp` `PgDn` | page up and down |
| `Home` `End`, `g` `G` | to the top and to the bottom |
| `Enter` | pin the selected node |
| `a` | return to automatic selection |
| `u` | update all subscriptions |
| `U` | update the subscription chosen with `Tab`; without a subscription filter, the subscription of the node under the cursor |
| `/` | filter by text (`Enter` applies, `Esc` clears) |
| `Tab`, `Shift+Tab` | filter by subscription, cycling |
| `Esc` | clear the filters (in search mode, only the text) |
| `?` or `F1` | help; any key closes it |
| `q`, `Ctrl+C` | quit |

In filter mode, letters, including `q`, go into the search line.

## Settings

For autocompletion and hints in the editor (VS Code with Even Better TOML, Taplo), add this line first in the file: `#:schema https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/config.schema.json`.

The settings file is TOML. In the service it is `/etc/raycat/config.toml`; in Docker, it is the
mounted file. An unknown key is an error, so a typo does not go unnoticed. raycat shows all
errors at once, with the field names. The file must be no larger than 1 MiB.

Durations are written as `500ms`, `30s`, `5m`, `6h`, `1d`; combinations are allowed (`1h30m`).
Sizes: `B`, `KiB`, `MiB`, `GiB` (`MB` is not understood). Case does not matter for units and for
value words (`happ`, `windows`, `debug`).

The reference is split into sections. A value marked "not set" means raycat chooses it itself.

<details>
<summary><code>[device]</code> — what is reported to the provider about the device</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `seed` | not set: the daemon creates a machine identifier in the state directory | a non-empty string up to 256 characters; cannot be combined with `machine_id` | A word by which the provider recognizes one device on any server. Variable: `RAYCAT_SEED` |
| `machine_id` | not set | 32 hexadecimal characters | The device identifier, if the provider already knows it |
| `hostname`, `model`, `manufacturer`, `os_version`, `locale` | not set: the daemon chooses the value | a non-empty string up to 128 characters, without line breaks or tabs | Values sent to the provider in the request headers |

```toml
[device]
seed = "any-word-the-same-on-all-servers"
```

</details>

<details>
<summary><code>[[subscription]]</code> — subscriptions</summary>

The first subscription is the main one. The others are backups, in the order they are declared.

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `name` | required | 1–64 characters, no `/` and no control characters; names must be unique | The subscription name in commands (`raycat fetch`, `raycat update`) and in `selection.pin` |
| `url` | required unless `url_file` is set | `https://…`; `http://` only with `allow_http = true`; up to 2048 characters; no login or password in the link | The subscription link. It is a secret: it is shortened in logs. Variable: `RAYCAT_SUBSCRIPTION` (first subscription only) |
| `url_file` | not set | path to a file up to 4 KiB containing one link; cannot be set together with `url` | A link read from a file, for example a Docker secret. Variable: `RAYCAT_SUBSCRIPTION_FILE` |
| `allow_http` | `false` | `true`, `false` | Allows `http://` for this subscription. Over http the link and the response are sent in the clear |
| `app` | required | `happ`, `incy` | The app that raycat emulates. Variable: `RAYCAT_APP` (first subscription only) |
| `platform` | required | `windows`, `android`; for `incy` only `android` | The emulated platform. Variable: `RAYCAT_PLATFORM` (first subscription only) |
| `seed` | not set: the shared `device.seed` | as for `device.seed` | The device of its own for this subscription |
| `update_interval` | the interval from the provider's response; if there is none, `12h` | from `10m` to `30d` | How often to update the subscription |
| `allow` | `[]`: all nodes | up to 256 masks | A whitelist. If the list is not empty, only the nodes matching at least one mask are used |
| `deny` | `[]` | up to 256 masks | A blacklist: nodes matching a mask are not used |
| `priority` | `[]` | up to 256 masks | Preferred nodes: the earlier a mask is in the list, the higher its priority |

A mask is compared with the node name (without the subscription name). `*` means any number of
characters, `?` exactly one. Case does not matter.

Instead of `url`, you can point to a file with the link (a Docker secret):

```toml
[[subscription]]
name = "main"
url_file = "/run/secrets/raycat_sub"
app = "happ"
platform = "windows"
```

</details>

<details>
<summary><code>[selection]</code> — node selection</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `check_url` | `https://www.gstatic.com/generate_204` | an `http://` or `https://` link | The address used to check whether a node responds |
| `check_interval` | `30s` | from `5s` to `10m` | How often nodes are checked |
| `failures` | `3` | an integer from `1` to `20` | How many consecutive checks without a response make a node unavailable; another one is then selected |
| `switch_gain` | `150ms` | from `0ms` to `60s` | How much faster a node of the same priority must be than the current one to switch to it. Two confirmations in a row are needed |
| `return_delay` | `5m` | from `0ms` to `24h` | How long the higher-priority node must work continuously before raycat switches back to it |
| `pin` | not set | `"subscription/node name"` | Always select this node, even when it does not respond. The node name is written exactly as in `raycat nodes --all`. A pin set with `raycat use` takes precedence over this key |

```toml
[selection]
failures = 3
return_delay = "5m"
```

</details>

<details>
<summary><code>[mode]</code> — operating mode</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `type` | `proxy` | `proxy`, `gateway` | `proxy` is a proxy for programs; `gateway` is a gateway for the traffic of the host, the containers and the devices. Variable: `RAYCAT_MODE` |
| `listen` | `127.0.0.1:7890` | `address:port` (IPv4 or `[IPv6]:port`), port not 0 | The proxy address. Applies only in `proxy`. Variable: `RAYCAT_LISTEN` |
| `auth` | not set | `login:password` or `off` | The login and the password for the proxy (HTTP and SOCKS5). The login has no `:`, up to 128 characters; the password 8 to 128 characters, no control characters. `off` means no password, including on a non-loopback address. Applies only in `proxy`. Variable: `RAYCAT_PROXY_AUTH` |
| `auth_file` | not set | a path to a file with the same line, at most 1 KiB | The same from a file, for example a Docker secret. Not set together with `auth`. Variable: `RAYCAT_PROXY_AUTH_FILE` |
| `kill_switch` | `true` | `true`, `false` | In `gateway` mode, does not let traffic go around the tunnel. Has no effect in `proxy`. Variable: `RAYCAT_KILL_SWITCH` |
| `lan` | `false` | `true`, `false` | A gateway for local network devices. Only in `gateway`. Variable: `RAYCAT_LAN` |
| `lan_interface` | the interface of the default route | up to 15 characters: Latin letters, digits, `-`, `_`, `.` | The interface from which the packets of the devices arrive. Used when `lan = true` |
| `lan_subnets` | the subnets of this interface | from 1 to 32 IPv4 subnets with a prefix from `/8` to `/31` | The subnets of the devices. Used when `lan = true` |

Keys that are set but do not apply to the selected mode are ignored. `raycat check` prints a
warning for each such key.

A proxy on an address that is not loopback (not `127.0.0.0/8` and not `::1`) does not start without
`auth` or `auth_file`, and reports an error. The exception is `auth = "off"`: then there is no
password, and the daemon writes a warning at start.

```toml
[mode]
type = "gateway"
kill_switch = true
```

</details>

<details>
<summary><code>[dns]</code> — DNS for xray</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `resolvers` | `["1.1.1.1", "8.8.8.8"]` | from 1 to 8 IP addresses | The DNS servers used by xray. Do not confuse it with the `dns:` key in Docker Compose |

```toml
[dns]
resolvers = ["1.1.1.1", "9.9.9.9"]
```

</details>

<details>
<summary><code>[routing]</code> — routing</summary>

For each connection the order is: your rules from top to bottom, then the `ru_direct` preset, then the
provider's rules (`provider`), and everything else goes through the VPN. The first matching rule
applies.

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `provider` | `false` | `true`, `false` | Apply the routing that the provider sends in the subscription response (see below) |
| `ru_direct` | `false` | `true`, `false` | The "Russia direct" preset: domains of the Russian zones (`ru`, `su`, `рф` and others) and Russian IPv4 subnets bypass the VPN. The list is built into raycat; the date of the subnet snapshot is shown by `raycat check` |
| `rule` | `[]` | up to 256 rules, `[[routing.rule]]` tables | Your own rules |

The preset's subnets apply only to connections by IP address. A domain outside the Russian zones
that points to a Russian address goes through the VPN. Direct domains (the preset and `direct`
rules) are resolved by the real DNS rather than fake-IP, so their queries go out directly, bypassing
the VPN.

The provider's routing (`provider = true`) is the rules that the provider sends in the subscription
response: in the `routing` header or as a `happ://routing/…` line in the body. The profile of the
first subscription in the settings that has one is applied. The rules come after `ru_direct`, in
this order: block, direct, through the VPN.

Translated:
- domain entries: `domain:`, `full:`, a bare domain (as `domain:`) and `keyword:`;
- IP addresses and subnets, IPv4 and IPv6;
- `geosite:category-ru` and `geosite:ru`: the Russian zones, as in the `ru_direct` preset;
- `geoip:ru`: Russian IPv4 subnets from the data built into raycat;
- `geoip:private` in the direct list: private networks already go directly.

Skipped; the daemon log shows the count and examples:
- `geosite:` and `geoip:` other than `ru` and `private`, and `ext:` files: raycat does not load geodata
  files;
- `regexp:`: xray checks regular expressions at start, and one bad entry would break the whole config;
- `geoip:private` in the block and proxy lists: private networks always go directly;
- entries that do not look like a domain or a subnet, including Cyrillic domains: write them in
  punycode (`xn--…`).

A profile with `global_proxy` and `happ://routing/off` gives no rules. raycat does not use the
provider's DNS servers or `dns_hosts`: DNS goes through raycat's own path. Direct domains of the
provider are resolved by the real DNS, as the preset's domains are. The log gets one line when the
profile appears or changes; `raycat check` shows `провайдер: «имя», правил N, пропущено M` in the
routing line.

Keys of one `[[routing.rule]]`:

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `domains` | `[]` | up to 4096 entries | Domains. `example.ru` is only that name; `*.example.ru` is the domain itself and all its subdomains. No `http://`, path or port. Latin letters, digits and "-", up to 253 characters; write Cyrillic names in punycode (`рф` is `xn--p1ai`) |
| `ips` | `[]` | up to 4096 entries | IPv4 or IPv6: a network in CIDR form (`203.0.113.0/24`, `2001:db8::/32`) or a single address (`198.51.100.7`) |
| `action` | required | `direct`, `proxy`, `block` | `direct` goes straight, bypassing the VPN; `proxy` goes through the VPN; `block` drops the traffic |

A rule needs at least one of `domains` and `ips`. A rule matches if any of its domains or any of
its addresses matches. Repeating an entry within one rule is not an error: raycat prints a warning
and counts the entry once.

```toml
[routing]
ru_direct = true

[[routing.rule]]
domains = ["example.ru", "*.bank.example"]
ips = ["203.0.113.0/24"]
action = "direct"

[[routing.rule]]
domains = ["*.stream.example"]
action = "proxy"
```

</details>

<details>
<summary><code>[xray]</code> — the core and its resources</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `path` | `/usr/libexec/raycat/xray` | a path up to 4096 bytes without control characters | Where the xray executable is located |
| `memory_limit` | `96MiB` | from `16MiB` to `16GiB` | The soft memory limit of xray |
| `tcp_congestion` | `auto` | `auto`, `off`, or the name of a kernel algorithm (`bbr`, `cubic`): lowercase Latin letters, digits, `-`, `_`, up to 15 characters | The TCP congestion control algorithm for connections to nodes. `auto` enables BBR only if the host allows it |
| `xhttp_connections` | not set: as the provider sets it | an integer from `1` to `16` | The number of parallel XHTTP connections |

```toml
[xray]
memory_limit = "128MiB"
xhttp_connections = 4
```

</details>

<details>
<summary><code>[log]</code> — log</summary>

| Key | Default | Allowed | What it does |
| --- | --- | --- | --- |
| `level` | `info` | `error`, `warn`, `info`, `debug` | The log verbosity. Variable: `RAYCAT_LOG` |

```toml
[log]
level = "debug"
```

</details>

### Environment variables

Variables override the settings file for the keys they relate to. An empty value counts as
not set. The `RAYCAT_INSTALL_*` variables are needed only by the installer's tests.

| Variable | What it sets |
| --- | --- |
| `RAYCAT_CONFIG` | The path to the settings file, if `--config` is not given. If the file does not exist, it is an error. Default: `/etc/raycat/config.toml`, if it exists |
| `RAYCAT_SUBSCRIPTION` | The `url` of the first subscription. If the file has no subscriptions, a subscription named `main` is created. Cannot be used together with `RAYCAT_SUBSCRIPTION_FILE` |
| `RAYCAT_SUBSCRIPTION_FILE` | The path to a file with the link of the first subscription (up to 4 KiB) |
| `RAYCAT_APP`, `RAYCAT_PLATFORM` | The app and the platform of the first subscription |
| `RAYCAT_SEED` | `device.seed`. Cannot be used together with `device.machine_id` from the file |
| `RAYCAT_MODE`, `RAYCAT_LISTEN`, `RAYCAT_KILL_SWITCH`, `RAYCAT_LAN`, `RAYCAT_PROXY_AUTH`, `RAYCAT_PROXY_AUTH_FILE`, `RAYCAT_LOG` | The corresponding keys of `[mode]` and `[log]` |
| `RAYCAT_STATE_DIR` | The state directory: machine identifier, subscription cache, pin. For root the default is `/var/lib/raycat`; for others, `$XDG_STATE_HOME/raycat` or `~/.local/state/raycat` |
| `RAYCAT_SOCKET` | The path of the API socket. For root the default is `/run/raycat/raycat.sock`; for others, `$XDG_RUNTIME_DIR/raycat.sock`, otherwise a file in the state directory |

### Recipes

**Several subscriptions with priority.** The main subscription is used while it has a live node.
The backup is selected when the main one has no live nodes. raycat decides when to return to the
main subscription, based on `return_delay`.

```toml
[[subscription]]
name = "main"
url = "https://…"
app = "happ"
platform = "windows"

[[subscription]]
name = "backup"
url = "https://…"
app = "happ"
platform = "android"
```

**Choosing a country.** `priority` sets the order of preference, `deny` removes service nodes.
To keep only one country, use `allow`.

```toml
[[subscription]]
name = "main"
url = "https://…"
app = "happ"
platform = "windows"
priority = ["*Germany*", "*Finland*"]
deny = ["*Info*"]
```

**Pinning a node.** The easiest way is the command `sudo raycat use "main/Germany 1"`; to remove
the pin, run `sudo raycat use auto`. A pin set by the command is kept across restarts and takes
precedence over `pin` from the file. `use auto` removes it as well. A pinned node is selected even
if it does not respond, so watch its status in `raycat status`.

```toml
[selection]
pin = "main/🇩🇪 Germany 1"
```

**One device for the provider across several servers.** Set the same word in `device.seed` on
every server. The provider will see one device instead of several.

```toml
[device]
seed = "one-word-for-all-servers"
```

**Russian sites direct.** Banks, government services and marketplaces often refuse connections
from foreign VPN addresses. The preset sends them around the VPN: the domains of Russian zones and
Russian IPv4 subnets.

```toml
[routing]
ru_direct = true
```

Things to know:

- Traffic to these sites leaves from the server's real address, and the sites see that address.
  That is what the preset is for. If the server's address must stay hidden from Russian sites too,
  do not turn the preset on.
- A Russian subnet may belong to a service that also serves foreign sites. Those sites will go
  direct. Exclude them with a rule `action = "proxy"`: it is checked before the preset.
- The subnets come from RIPE NCC registrations. They show where an address is registered, not
  where the device is.

**Your own rules.** Block an advertising domain, open a home bank directly:

```toml
[[routing.rule]]
domains = ["ads.example.com"]
action = "block"

[[routing.rule]]
domains = ["*.home-bank.example"]
action = "direct"
```

## Installing on a server

> [!NOTE]
> The first version has not been released yet: the installer will work once releases exist.
> Until the project has a stable release, `--channel dev` installs the latest dev build.

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

The installer picks the build for your CPU (x86_64, aarch64, armv7; on CPUs without AES, such as
Raspberry Pi 3/4, it takes the `-noaes` xray core), verifies the checksum against `SHA256SUMS`,
and if anything does not match, it changes nothing. If `gh` is installed and signed in, it also
verifies the provenance of the archive (`gh attestation verify`). Then it lays out the files:

| What | Where |
| --- | --- |
| program | `/usr/local/bin/raycat` |
| xray core | `/usr/libexec/raycat/xray` |
| settings | `/etc/raycat/config.toml` (0600, an existing file is left untouched) |
| state | `/var/lib/raycat` |
| service | `/etc/systemd/system/raycat.service` |
| completions, man page, license | `/usr/local/share/…` |

The installer does not change the host's network settings: the daemon sets up gateway mode
itself, from the settings file. If systemd is present, the service is enabled at boot. On the
first install it is not started yet: put the subscription link into `/etc/raycat/config.toml` (an
example with explanations is created for you) and run `sudo systemctl start raycat`. After that,
`sudo raycat status`, `nodes`, `tui` and `journalctl -u raycat -f` work. Without systemd, the
installer prints the command to start the daemon by hand (`raycat daemon`).

- **The dev channel** (the latest build from `main`, for experienced users): add the options after
  `sh -s --`: `curl -fsSL … | sudo sh -s -- --channel dev`. A specific version: `--version 1.2.3`.
- **A CPU without AES**: `--noaes` forces the `-noaes` xray build, `--no-noaes` forces it not to be
  installed. Without options, the choice is made from `/proc/cpuinfo`.
- **Updating**: run the installation again. Settings are kept; the service restarts only if the
  files changed.
- **Removing**: `sudo sh install.sh --uninstall`. Settings and state stay; `--uninstall --purge`
  deletes them too (the provider will see a new device after a fresh install).
- **All options**: `sh install.sh --help`.

## Gateway for Docker containers

The apps live in the network namespace of the raycat container (`network_mode: service:raycat`)
and reach the internet only through the tunnel. Recommended settings, with the gateway itself
strictly isolated:

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

- **Permissions of the settings file.** With `cap_drop: [ALL]`, root in the container does not bypass
  file permissions, so a file with mode 600 owned by another host user cannot be read. Make it
  readable: `chmod 644 config.toml` (and keep the directory closed to others), or hand it to root:
  `sudo chown 0:0 config.toml`, mode 600.
- **The state and the API socket** live in the `/var/lib/raycat` volume, so the container root stays
  read-only. The commands run inside the container: `docker compose exec raycat raycat status`.
- **`dns`** is required: Docker queries its resolver from the container's namespace, where the queries
  are intercepted; without an explicit address the gateway refuses to start (names would otherwise
  resolve around the tunnel). This is the Docker Compose key, not `[dns]` in `config.toml`.
- **Readiness.** The image runs `raycat health` every 10 s: the daemon responds, xray runs and a node
  is selected; the first start gets 60 s (a panel may respond slowly). With
  `condition: service_healthy` the apps do not start until the VPN is ready, and `restart: true`
  restarts them together with the gateway. To check by hand:
  `docker compose exec raycat raycat health` (exit code 0 or 1 and the reason).

**The subscription link as a Docker secret.** Instead of `url`, set `url_file = "/run/secrets/raycat_sub"`
and pass the file through `secrets:` (spaces and line breaks at the edges do not matter; the file
permissions are the same as for `config.toml`):

```yaml
services:
  raycat:
    secrets: [raycat_sub]     # for the raycat service; the rest is unchanged
secrets:
  raycat_sub:
    file: ./raycat_sub.txt    # a file with one line: the subscription link
```

## Gateway for the local network

raycat can be a gateway for network devices: TVs, phones and consoles that have no VPN of their
own. The devices reach the internet through the host running raycat, which intercepts their TCP
and UDP traffic and sends it into the tunnel.

**Running it.** raycat must run in the network namespace of the host itself: as a systemd service,
or in Docker with `network_mode: host`. It needs the `CAP_NET_ADMIN` capability (a service running
as root, or `cap_add: [NET_ADMIN]`).

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

**Devices.** Set the host's address as the gateway and the DNS server: on the device itself, or in
the DHCP settings of the router that hands out the addresses. The DNS of the devices (any server,
port 53) also goes through raycat, so names resolve to placeholder addresses (fake-IP, placeholder
addresses for domain names: the connection goes through the tunnel by name), just as on the host
itself. IPv6 of the devices is blocked: turn it off on the device or in the router, or the device
will bypass the host.

**The host needs nothing else.** Do not enable forwarding (`ip_forward`): intercepted packets are
delivered locally, and raycat does not change sysctl. If forwarding is enabled on the host for other
reasons, the kill switch still will not let the devices out past the tunnel.

**The host stays reachable.** Incoming connections to the host (SSH and other services) and the
replies to them are left alone at start, when xray fails and at stop. The host's own addresses,
private networks (the Docker network and other devices of your network included), multicast and
broadcast are not intercepted either. On a clean stop, raycat removes its rules; after a crash they
stay in place to hold the kill switch, and the next start installs them again.

## Performance

Everything here is optional and is set in the `[xray]` section of the settings file:

```toml
[xray]
memory_limit = "96MiB"      # default
tcp_congestion = "auto"     # auto (default), off, or an algorithm name
xhttp_connections = 4       # 1 to 16; not set means the provider's setting
```

- `memory_limit` is a soft memory limit for xray. A limit that is too low keeps the Go garbage
  collector running constantly and loads the CPU under traffic.
- `tcp_congestion` is TCP congestion control for connections to nodes. `auto` enables BBR only if the
  host definitely allows it (the algorithm is loaded into the kernel and permitted for processes, or
  xray has `CAP_NET_ADMIN`); otherwise it logs why it did not enable it. BBR speeds up transfers over
  links with packet loss. This setting does not affect QUIC nodes (Hysteria2, XHTTP over h3): Hysteria2
  already uses BBR by default.
- `xhttp_connections` is the number of parallel XHTTP connections. It helps when a provider limits the
  speed of a single connection; the cost is more CPU load and more connections to the server. Without a
  value, the provider's settings apply (if there are none, xray uses 3 connections). Nodes where the
  provider set `xmux` itself are not changed.
- For QUIC and UDP nodes (Hysteria2, XHTTP over h3, mKCP), the daemon warns once per start if the host's
  UDP buffers are smaller than 7.5 MiB: then it is worth raising `net.core.rmem_max` and
  `net.core.wmem_max` on the host.
- CPUs without hardware AES (Raspberry Pi 3/4 and others: there is no `aes` flag in `/proc/cpuinfo`) get
  an xray built for fast AES-GCM. The installer picks it automatically, see `--noaes` in
  [installation](#installing-on-a-server).

## FAQ and troubleshooting

1. **The provider does not return nodes, or all the nodes turned out to be stubs.** Run
   `raycat fetch main` (the name of your subscription): the command shows the response, the expiry
   date and the remaining traffic.
   - `HTTP 403` («панель блокирует этот клиент или требует HWID» (the panel blocks this client or
     requires HWID)) and `HTTP 404` («подписка не найдена, либо панель требует HWID» (the subscription
     was not found, or the panel requires HWID)) mean the provider does not accept the emulation. Check
     `app` and `platform`: the provider may issue nodes only to the app you emulate.
   - «достигнут лимит устройств» (the device limit has been reached): all the provider's slots are
     taken. Free a slot in the panel, or use a device `seed` that the provider already knows.
   - «все узлы — заглушки (адрес 0.0.0.0 или локальный)» (all nodes are stubs (address 0.0.0.0 or
     local)): the provider returned only message nodes. Check the subscription's expiry date and the
     remaining traffic in the panel.

2. **«демон не запущен» (the daemon is not running), or `raycat status` cannot see the daemon.** Check
   the service: `sudo systemctl status raycat` and `journalctl -u raycat -n 100`. After the first install
   the service does not start by itself: put in the subscription link and run `sudo systemctl start raycat`.
   After changing the settings, run `sudo systemctl restart raycat`. In a container, read the log with
   `docker compose logs -f raycat`.

3. **«нет прав на сокет … (попробуйте sudo)» (no permission for the socket … (try sudo)).** Client commands
   can be run only by the daemon's user and by root. Run them like this: `sudo raycat status`. In a
   container: `docker compose exec raycat raycat status`.

4. **In Docker, the gateway does not read `config.toml` (`Permission denied`).** This is a consequence of
   `cap_drop: [ALL]`: container root does not bypass file permissions. Make the file readable (`chmod 644`)
   or hand it to root (`chown 0:0`, mode 600). Details are in the [Docker section](#gateway-for-docker-containers).

5. **The gateway in Docker does not start.** Read the message:
   - «режиму шлюза нужна привилегия CAP_NET_ADMIN» (the gateway mode needs the CAP_NET_ADMIN privilege):
     add `cap_add: [NET_ADMIN]`.
   - «Docker разрешает имена для этого контейнера через резолвер хоста» (Docker resolves names for this
     container through the host's resolver): add `dns: [1.1.1.1]` (any external DNS) to the service.

6. **Apps in Docker start without the VPN or stay without network.** The apps must wait for
   `condition: service_healthy`, as in the example. The gateway's first start can take up to a minute:
   the image waits 60 s for the subscription to provide a working node. Known limitation: if the gateway
   was restarted with `docker restart` rather than `docker compose restart`, the apps may stay without
   network. In that case, restart them manually.

7. **Slow on Raspberry Pi 3/4.** These CPUs have no hardware AES. The installer picks the `-noaes` build
   itself; manually, use `--noaes` at installation, or the `:latest-noaes` images (arm64 only). See
   [performance](#performance).

8. **The provider's device changed after a reinstall.** The machine identifier is stored in
   `/var/lib/raycat/machine-id`. `--uninstall --purge` deletes it, and a new installation creates a new
   device. To keep the device, set `[device] seed` to the same word, or do not delete the state directory.
   A plain `--uninstall` keeps it.

9. **«http небезопасен: используйте https или добавьте allow_http = true» (http is insecure: use https or
   add allow_http = true).** A subscription over `http://` is not accepted without `allow_http = true` in
   its block. It is better to get a link over https. If the provider gives only http, enable `allow_http`:
   the link and the response will then travel in plain text.

10. **«узел не выбран: подписки ещё не дали рабочих узлов» (no node is selected: the subscriptions have not
    yet provided working nodes).** `raycat nodes --all` shows all the nodes and their status. If the `allow`
    or `deny` masks filtered out everything, loosen them. If a pinned node is selected and does not respond,
    remove the pin: `sudo raycat use auto`.

## Security

- **Kill switch.** In `gateway` mode with `kill_switch = true` (the default), the traffic of devices,
  containers and the host that the tunnel did not intercept does not leave: neither directly, nor through
  DNS, nor over IPv6 or ICMP. If xray fails, the intercepted traffic is refused rather than sent around the VPN.
- **Proxy mode is not a protection.** The kill switch does not apply there: programs that do not use the
  proxy connect directly.
- **Secrets.** The subscription link and `seed` are stored in `config.toml`; the installer creates the file
  with mode 0600. In logs and error messages the link is shortened to the host and the last four characters
  of the path. The state directory `/var/lib/raycat` is accessible only to its owner (0700).
- **Device identifier.** `raycat identity` prints the HWID in full, while `raycat fetch` hides it. Do not
  publish the output of `identity` in public places.
- **Proxy without a password.** The default address `127.0.0.1` is reachable only from this server. An
  address that is not loopback does not start without `auth` or `auth_file`: otherwise anyone who can
  reach it exits through your VPN. `auth = "off"` lifts this requirement, at your own risk.
- Report vulnerabilities privately: [SECURITY.md](.github/SECURITY.md).

## Versions and channels

Releases are automatic; each channel has its own tags.

| Channel | What it is | Docker image | Files |
| --- | --- | --- | --- |
| **stable** | Working versions `vX.Y.Z`. They went through the dev channel and waited out the hold period: one day for changes to emulation profiles, three days for everything else. Breaking changes are released only by hand | `ghcr.io/raycat-app/raycat:latest`, `:X.Y.Z`, `:X.Y` | [Releases](https://github.com/raycat-app/raycat/releases/latest) |
| **dev** | A build of each merge into `main` after green CI, `vX.Y.Z-dev.N`. For testing ahead of time, not for production servers; the last 20 are kept | `:dev`, `:X.Y.Z-dev.N` | a pre-release on the Releases page |

Images are multi-architecture (amd64 and arm64). For **Raspberry Pi 3/4 and other CPUs without hardware
AES** (there is no `aes` flag in `/proc/cpuinfo`) there are variants with xray built for fast AES-GCM on
such hardware: the images `:latest-noaes`, `:X.Y.Z-noaes`, `:dev-noaes` (arm64 only) and the archives
with `-noaes` in the name (aarch64 and armv7).

If an open issue carries the `стоп-релиз` (stop-release) label, promotion from dev to stable is paused.

### Verifying authenticity

Archives and images carry build provenance attestations (GitHub); images also carry an SBOM.
Verification needs the [GitHub CLI](https://cli.github.com/):

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-X.Y.Z-x86_64-linux-musl.tar.gz --repo raycat-app/raycat
gh attestation verify oci://ghcr.io/raycat-app/raycat:latest --repo raycat-app/raycat
```

## Contributing

Suggestions and fixes are welcome: see [CONTRIBUTING.md](.github/CONTRIBUTING.md) (in Russian).
Report vulnerabilities privately: [SECURITY.md](.github/SECURITY.md).

## License

[MIT](LICENSE).
