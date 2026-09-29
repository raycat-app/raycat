<div align="center">

<img src="assets/logo-512.png" alt="raycat" width="200">

# raycat

**VPN-подписка для сервера: шлюз и прокси для хоста и Docker-контейнеров**

[![CI](https://github.com/raycat-app/raycat/actions/workflows/ci.yml/badge.svg)](https://github.com/raycat-app/raycat/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/raycat-app/raycat/badge)](https://scorecard.dev/viewer/?uri=github.com/raycat-app/raycat)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

Русский · [English](README.en.md)

</div>

> [!IMPORTANT]
> raycat в разработке, первая версия ещё не выпущена.

## Что это

raycat берёт вашу VPN-подписку и делает из неё надёжный выход в интернет для
сервера:

- **весь хост** — служба systemd;
- **Docker-контейнеры** — шлюз (`network_mode: service:raycat`) или HTTP/SOCKS-прокси,
  без настроек внутри приложений;
- **локальная сеть** — шлюз для устройств.

Подписку raycat получает так же, как приложение Happ для Windows или Android
(или INCY для Android), поэтому работает с подписками, которые провайдеры выдают
только этим приложениям. Трафик идёт через [xray-core](https://github.com/XTLS/Xray-core) —
то же ядро, что внутри Happ.

## Возможности первой версии

- Несколько подписок с приоритетами: основная и резервные.
- Автоматическое переключение на живой узел по вашим правилам: приоритеты
  подписок и узлов, чёрный и белый списки, без лишних прыжков между узлами.
- Kill switch: пока VPN не работает, трафик не уходит напрямую, DNS тоже.
- Командная строка с понятной справкой и TUI с теми же возможностями.
- Статические бинарники для x86_64, arm64 и armv7, образ Docker, установка одной
  командой.

## Участие

Предложения и исправления приветствуются: см. [CONTRIBUTING.md](CONTRIBUTING.md).
Об уязвимостях сообщайте приватно: [SECURITY.md](SECURITY.md).

## Лицензия

[MIT](LICENSE).
