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

## Contributing

Suggestions and fixes are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) (in
Russian). Report vulnerabilities privately: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).
