<div align="center">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
  <img src="assets/logo.svg" alt="raycat" width="200">
</picture>

# raycat

**VPN-подписка для Linux-сервера.**<br>
Нужные контейнеры, весь сервер или домашняя сеть выходят в интернет через неё.

[![Версия](https://img.shields.io/github/v/release/raycat-app/raycat?include_prereleases&style=flat-square&label=версия&color=2d333b)](https://github.com/raycat-app/raycat/releases)
[![Лицензия](https://img.shields.io/badge/лицензия-MIT-2d333b?style=flat-square)](../LICENSE)

[Запуск](#запуск) · [Режимы](#режимы) · [Управление](#управление) · [Настройки](#настройки) · [Вопросы](#вопросы-и-ошибки) · [English](README.en.md)

[Сообщить об ошибке](https://github.com/raycat-app/raycat/issues/new?template=bug.yml) · [Предложить идею](https://github.com/raycat-app/raycat/issues/new?template=feature.yml)

<br>

<img src="assets/tui.png" alt="raycat tui: обзор" width="820">

</div>

<br>

Вы даёте raycat ссылку на подписку, и он держит туннель на сервере. Настраивать сами приложения не нужно. Если узел умер, raycat переключится на живой. Если живых не осталось, трафик не уйдёт мимо VPN.

Многие провайдеры отдают подписку только своим приложениям. raycat запрашивает её так же, как Happ (Windows и Android) или INCY (Android), а туннель строит на [xray-core](https://github.com/XTLS/Xray-core).

> [!NOTE]
> Стабильной версии пока нет. До её выхода используйте образ `ghcr.io/raycat-app/raycat:dev` и установку с `--channel dev`.

## Запуск

### В Docker

Через VPN пойдут только контейнеры, которые вы подключите к raycat. Остальной сервер и другие контейнеры работают как обычно.

`raycat/config.toml`:

```toml
[[subscription]]
url = "https://…"       # ссылка подписки
app = "happ"            # какое приложение принимает провайдер: happ или incy
platform = "windows"    # windows или android (у incy только android)

[mode]
type = "gateway"
```

> [!CAUTION]
> Ссылка подписки работает как пароль. Не публикуйте её и не храните в открытых репозиториях.

`compose.yml` рядом с каталогом `raycat`. Контейнер `app` здесь ваше приложение:

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
    image: ваше-приложение
    network_mode: service:raycat   # эта строка пускает контейнер через VPN
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

### На сервере

```sh
curl -fsSL https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh | sudo sh
sudo raycat init --force    # спросит ссылку, приложение и режим
sudo systemctl start raycat
sudo raycat tui
```

<p align="right"><a href="#raycat">наверх</a></p>

## Режимы

| Что идёт через VPN | Как запустить | Настройка |
| --- | --- | --- |
| **Отдельные контейнеры** | raycat в Docker, у контейнеров `network_mode: service:raycat` | `type = "gateway"` |
| **Весь сервер** | raycat службой на сервере | `type = "gateway"` |
| **Отдельные программы** | прокси HTTP и SOCKS5 на `127.0.0.1:7890` | `type = "proxy"` |
| **Домашняя сеть** | raycat в сети хоста, сервер указан шлюзом на устройствах | `type = "gateway"`, `lan = true` |

> [!IMPORTANT]
> Kill switch работает в режиме `gateway`: без туннеля трафик и DNS не выходят наружу. В режиме `proxy` программы, которые не используют прокси, ходят напрямую.

**Ещё raycat умеет:**

- держать основную и резервные подписки и менять их порядок на лету;
- проверять узлы и переключаться при сбое, закреплять узел или исключать узлы масками вроде `Обход*`;
- пускать российские сайты мимо VPN (`ru_direct = true`);
- работать там же, где и ваш сервер: сборки для x86_64, arm64 и armv7, в том числе Raspberry Pi.

<p align="right"><a href="#raycat">наверх</a></p>

## Управление

`raycat tui` показывает всё на одном экране и работает с мышью. Настройки можно менять прямо там.

<div align="center">
  <img src="assets/tui-nodes.png" alt="raycat tui: узлы" width="820">
</div>

<details>
<summary><b>Вкладка «Настройки»</b></summary>

<br>

<img src="assets/tui-settings.png" alt="raycat tui: настройки" width="820">

</details>

<br>

| Клавиша | Вкладка | Что там можно сделать |
| --- | --- | --- |
| <kbd>1</kbd> | Обзор | увидеть состояние туннеля, обновить подписки, проверить скорость |
| <kbd>2</kbd> | Узлы | закрепить узел <kbd>p</kbd>, исключить <kbd>x</kbd>, скрыть мёртвые <kbd>d</kbd>, проверить скорость <kbd>t</kbd> |
| <kbd>3</kbd> | Подписки | изменить маски <kbd>m</kbd> и порядок <kbd>[</kbd> <kbd>]</kbd> |
| <kbd>4</kbd> | Журнал | посмотреть события, только предупреждения <kbd>w</kbd> |
| <kbd>5</kbd> | Настройки | поменять основные настройки |

То же самое есть в командах:

| Команда | Что делает |
| --- | --- |
| `raycat status` | состояние туннеля, текущий узел, подписки |
| `raycat nodes` | список узлов |
| `raycat use <узел>`, `raycat use auto` | закрепить узел, вернуть автовыбор |
| `raycat update` | обновить подписки сейчас |
| `raycat speedtest` | проверить скорость через VPN |
| `raycat check` | проверить файл настроек |
| `raycat config` | показать итоговые настройки, секреты скрыты |
| `raycat reload` | применить правку файла настроек |

Остальное покажет `raycat --help`.

<p align="right"><a href="#raycat">наверх</a></p>

## Настройки

Настройки лежат в одном файле: `/etc/raycat/config.toml` на сервере, `raycat/config.toml` в примере с Docker. Все ключи с пояснениями есть в [примере настроек](../deploy/config.example.toml), при опечатке raycat подскажет правильное название.

> [!TIP]
> После правки выполните `sudo raycat reload`. Демон применит то, что можно применить на лету, а про остальное скажет, что нужен перезапуск.

<details>
<summary><b>Свои правила: что пускать мимо VPN</b></summary>

<br>

Обычно весь трафик идёт через VPN. Правила нужны для исключений. Например, открывать сайт банка напрямую, а один российский сайт, наоборот, через VPN:

```toml
[routing]
ru_direct = true

[[routing.rule]]
domains = ["mybank.example"]
action = "direct"             # direct: мимо VPN

[[routing.rule]]
domains = ["*.example.ru"]    # *.example.ru: сам сайт и его поддомены
action = "proxy"              # proxy: через VPN, block: заблокировать
```

Правила проверяются сверху вниз, срабатывает первое подошедшее. Они важнее `ru_direct`. Вместо `domains` или вместе с ними можно указать `ips`: подсети или отдельные адреса.

</details>

<details>
<summary><b>Домашняя сеть</b></summary>

<br>

raycat должен работать в сети самого хоста: как служба или в Docker с `network_mode: host`.

```toml
[mode]
type = "gateway"
lan = true
```

На устройстве или в DHCP роутера укажите адрес сервера как шлюз и DNS. Включать `ip_forward` не нужно, SSH и другие службы хоста продолжат работать. `raycat check` покажет, какой интерфейс и какие подсети будут перехватываться.

</details>

<details>
<summary><b>Переменные окружения</b></summary>

<br>

Перекрывают файл настроек, удобно в Docker.

| Переменная | Что задаёт |
| --- | --- |
| `RAYCAT_SUBSCRIPTION`, `RAYCAT_SUBSCRIPTION_FILE` | ссылку первой подписки или файл с ней |
| `RAYCAT_APP`, `RAYCAT_PLATFORM` | приложение и платформу первой подписки |
| `RAYCAT_MODE`, `RAYCAT_KILL_SWITCH`, `RAYCAT_LAN`, `RAYCAT_LISTEN` | режим работы |
| `RAYCAT_PROXY_AUTH`, `RAYCAT_PROXY_AUTH_FILE` | логин и пароль прокси |
| `RAYCAT_SEED`, `RAYCAT_LOG` | устройство и уровень журнала |
| `RAYCAT_CONFIG`, `RAYCAT_STATE_DIR`, `RAYCAT_SOCKET` | пути |

</details>

<details>
<summary><b>Если что-то не работает</b></summary>

<br>

**Провайдер не отдаёт узлы.** Запустите `raycat fetch <подписка>`: команда покажет ответ провайдера. Ошибки 403 и 404 обычно значат, что нужно другое приложение или платформа. Ещё бывает, что исчерпан лимит устройств или закончился срок подписки.

**`raycat status` пишет, что демон не запущен.** Посмотрите `sudo systemctl status raycat` и `journalctl -u raycat -n 100`. Клиентские команды нужно запускать через `sudo`.

**Шлюз в Docker не стартует.** Нужны `cap_add: [NET_ADMIN]` и `dns: [1.1.1.1]`, как в примере.

**Настройки из TUI не сохраняются в Docker.** Монтируйте каталог, а не отдельный файл, как в примере.

**Медленно на Raspberry Pi 3 и 4.** У этих процессоров нет аппаратного AES. Установщик сам ставит подходящую сборку, а в Docker добавьте к тегу `-noaes`, например `:latest-noaes`.

</details>

<details>
<summary><b>Обновления и проверка сборок</b></summary>

<br>

Есть два канала. `dev` собирается из каждого изменения. `stable` выходит сам, когда сборка выдержала срок на dev. Обновления xray и исправления уязвимостей выходят сразу. Обновить установку на сервере можно той же командой установки, настройки сохранятся. Удалить: `sudo sh install.sh --uninstall`.

Архивы и образы подписаны аттестациями GitHub:

```sh
gh attestation verify raycat-X.Y.Z-x86_64-linux-musl.tar.gz --repo raycat-app/raycat
gh attestation verify oci://ghcr.io/raycat-app/raycat:latest --repo raycat-app/raycat
```

</details>

<p align="right"><a href="#raycat">наверх</a></p>

## Что внутри

[![Rust](https://img.shields.io/badge/Rust-2d333b?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/) [![Xray-core](https://img.shields.io/badge/Xray--core-2d333b?style=flat-square)](https://github.com/XTLS/Xray-core) [![Ratatui](https://img.shields.io/badge/Ratatui-2d333b?style=flat-square)](https://ratatui.rs/) [![Docker](https://img.shields.io/badge/Docker-2d333b?style=flat-square&logo=docker&logoColor=white)](https://www.docker.com/) [![nftables](https://img.shields.io/badge/nftables-2d333b?style=flat-square)](https://www.netfilter.org/projects/nftables/)

Сам raycat написан на Rust. Туннель строит [Xray-core](https://github.com/XTLS/Xray-core), интерфейс сделан на [Ratatui](https://ratatui.rs/), перехват трафика работает через nftables. Спасибо этим проектам.

## Что дальше

- [x] Шлюз для контейнеров, сервера и домашней сети
- [x] Запросы как у Happ и INCY, профили обновляются сами
- [x] Интерфейс в терминале с мышью и настройками на лету
- [ ] Первая стабильная версия 0.1.0
- [ ] Версия 1.0

<p align="right"><a href="#raycat">наверх</a></p>

## Вопросы и ошибки

Вопросы задавайте в [обсуждениях](https://github.com/raycat-app/raycat/discussions), ошибки присылайте в [issues](https://github.com/raycat-app/raycat/issues). Никогда не публикуйте ссылку подписки и вывод `raycat identity`. Об уязвимостях сообщайте [приватно](https://github.com/raycat-app/raycat/security/advisories/new).

Лицензия [MIT](../LICENSE).
