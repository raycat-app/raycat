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
- Для Raspberry Pi 3/4 и других процессоров без аппаратного AES будет отдельный
  вариант образа (тег `-noaes`) с более быстрым шифрованием на таком железе.

## Установка на сервер

> [!NOTE]
> Первая версия ещё не выпущена: установщик заработает, когда появятся выпуски.
> Пока в проекте нет ни одного stable-выпуска, `--channel dev` ставит последнюю
> dev-сборку.

Нужны root, `curl` или `wget`, `tar` и `sha256sum`. Одна команда:

```sh
curl -fsSL https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh | sudo sh
```

Можно сначала скачать скрипт и посмотреть, что он делает:

```sh
curl -fsSLO https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/install.sh
less install.sh
sudo sh install.sh --dry-run   # план без изменений
sudo sh install.sh
```

Установщик выбирает сборку под процессор (x86_64, aarch64, armv7; на процессорах
без AES вроде Raspberry Pi 3/4 берёт ядро xray `-noaes`), проверяет контрольную
сумму по `SHA256SUMS` и при любом несовпадении ничего не меняет. Если установлен и
авторизован `gh`, он дополнительно проверяет происхождение архива
(`gh attestation verify`). Затем он раскладывает файлы:

| Что | Где |
| --- | --- |
| программа | `/usr/local/bin/raycat` |
| ядро xray | `/usr/libexec/raycat/xray` |
| настройки | `/etc/raycat/config.toml` (0600, существующий файл не трогается) |
| состояние | `/var/lib/raycat` |
| служба | `/etc/systemd/system/raycat.service` |
| автодополнение, man, лицензия | `/usr/local/share/…` |

Сетевые настройки хоста установщик не меняет: режим шлюза настраивает сам демон по
файлу настроек. Если systemd есть, служба включается в автозапуск. При первой
установке она ещё не запускается: впишите ссылку подписки в
`/etc/raycat/config.toml` (пример с пояснениями создаётся сам) и выполните
`sudo systemctl start raycat`. Дальше работают `sudo raycat status`, `nodes`, `tui` и
`journalctl -u raycat -f`. Без systemd установщик подскажет команду для ручного
запуска (`raycat daemon`).

- **Канал dev** (последняя сборка из `main`, для опытных): добавьте параметры после
  `sh -s --`: `curl -fsSL … | sudo sh -s -- --channel dev`. Конкретная версия:
  `--version 1.2.3`.
- **Обновление**: повторите установку. Настройки сохраняются, служба
  перезапускается, только если файлы изменились.
- **Удаление**: `sudo sh install.sh --uninstall`. Настройки и состояние остаются;
  `--uninstall --purge` удаляет и их (устройство для провайдера при новой установке
  создастся заново).
- **Все параметры**: `sh install.sh --help`.

## Производительность

Всё необязательно и задаётся в разделе `[xray]` файла настроек:

```toml
[xray]
memory_limit = "96MiB"      # по умолчанию
tcp_congestion = "auto"     # auto (по умолчанию), off или имя алгоритма
xhttp_connections = 4       # от 1 до 16; не задано — как у провайдера
```

- `memory_limit` — мягкий лимит памяти xray. Слишком малый заставляет сборщик мусора
  Go работать постоянно и грузит процессор под нагрузкой.
- `tcp_congestion` — управление перегрузкой TCP для соединений к узлам. `auto`
  включает BBR, только если хост это точно позволяет (алгоритм загружен в ядро и
  разрешён процессам или у xray есть `CAP_NET_ADMIN`), иначе пишет в лог, почему
  не включил. BBR ускоряет передачу на каналах с потерями. QUIC-узлы (Hysteria2,
  XHTTP поверх h3) этой настройки не касается: Hysteria2 по умолчанию уже работает
  на BBR.
- `xhttp_connections` — число параллельных соединений XHTTP. Помогает, когда
  провайдер ограничивает скорость одного соединения; цена — больше нагрузка на
  процессор и больше соединений к серверу. Без значения действуют настройки
  провайдера (если их нет, xray берёт 3 соединения). Узлы, где провайдер сам задал
  `xmux`, не меняются.
- Для узлов на QUIC и UDP (Hysteria2, XHTTP поверх h3, mKCP) демон один раз за запуск
  предупреждает, если буферы UDP хоста меньше 7.5 МиБ: тогда стоит поднять
  `net.core.rmem_max` и `net.core.wmem_max` на хосте.

## Шлюз для Docker-контейнеров

Приложения живут в сетевом пространстве контейнера raycat (`network_mode:
service:raycat`) и выходят в интернет только через туннель. Рекомендуемые настройки,
с жёсткой изоляцией самого шлюза:

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
    image: ваше-приложение
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
name = "основная"
url = "https://…"
app = "happ"
platform = "windows"

[mode]
type = "gateway"
```

- **Права файла настроек.** При `cap_drop: [ALL]` root в контейнере не обходит права
  доступа, поэтому файл с правами 600 другого пользователя хоста не читается. Сделайте
  его читаемым: `chmod 644 config.toml` (каталог закройте от посторонних) или передайте
  root: `sudo chown 0:0 config.toml`, права 600.
- **Состояние и сокет API** лежат в томе `/var/lib/raycat`: корень контейнера остаётся
  только для чтения. Команды работают внутри контейнера:
  `docker compose exec raycat raycat status`.
- **`dns`** нужен: Docker опрашивает свой резолвер из пространства контейнера, где
  запросы перехватываются; без явного адреса шлюз откажется стартовать (иначе имена
  разрешались бы мимо туннеля).
- **Готовность.** Образ сам проверяет `raycat health` каждые 10 с: демон отвечает, xray
  работает, узел выбран; на первый старт даётся 60 с (панель может отвечать медленно).
  С `condition: service_healthy` приложения не стартуют, пока VPN не готов, а
  `restart: true` перезапускает их вместе со шлюзом. Проверить вручную:
  `docker compose exec raycat raycat health` (код 0 или 1 и причина).

## Шлюз для локальной сети

raycat может быть шлюзом для устройств сети: телевизоров, телефонов, консолей,
у которых нет своего VPN. Устройства ходят в интернет через хост с raycat, а он
перехватывает их TCP и UDP и отправляет в туннель.

**Запуск.** raycat должен работать в сетевом пространстве самого хоста: службой
systemd или в Docker с `network_mode: host`. Нужна привилегия `CAP_NET_ADMIN`
(служба от root или `cap_add: [NET_ADMIN]`).

```toml
# /etc/raycat/config.toml
[mode]
type = "gateway"
lan = true
# lan_interface = "eth0"                # по умолчанию: интерфейс маршрута по умолчанию
# lan_subnets = ["192.168.1.0/24"]      # по умолчанию: подсети этого интерфейса
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

`raycat check` покажет, какие интерфейс и подсети будут перехватываться.

**Устройства.** Адрес хоста указывается шлюзом и DNS-сервером: на самом
устройстве или в DHCP роутера, который раздаёт адреса. DNS устройств (любой
сервер, порт 53) тоже идёт через raycat, поэтому имена получают адреса-заглушки
(fake-IP), как и у самого хоста. IPv6 устройств блокируется: отключите его на
устройстве или в роутере, иначе устройство пойдёт мимо хоста.

**Хосту ничего больше не нужно.** Пересылку (`ip_forward`) включать не надо:
перехваченные пакеты доставляются локально, raycat не меняет sysctl. Если пересылка
на хосте включена по другим причинам, kill switch всё равно не выпустит устройства
наружу мимо туннеля.

**Хост остаётся доступным.** Входящие соединения к хосту (SSH и другие службы) и
ответы на них правила не трогают ни при запуске, ни при падении xray, ни при
остановке. Не перехватываются также адреса самого хоста, приватные сети (в том числе
сеть Docker и другие устройства сети), multicast и broadcast. При штатной остановке
raycat снимает свои правила; после аварии они остаются, чтобы держать kill switch,
а следующий запуск ставит их заново.

## Версии и каналы

Выпуск автоматический, у каждого канала свои теги.

| Канал | Что это | Образ Docker | Файлы |
| --- | --- | --- | --- |
| **stable** | Рабочие версии `vX.Y.Z`. Прошли канал dev и выдержали срок: изменения профилей эмуляции — сутки, остальные — трое суток. Несовместимые изменения выходят только вручную | `ghcr.io/raycat-app/raycat:latest`, `:X.Y.Z`, `:X.Y` | [Releases](https://github.com/raycat-app/raycat/releases/latest) |
| **dev** | Сборка каждого слияния в `main` после зелёного CI, `vX.Y.Z-dev.N`. Для проверки заранее, не для рабочих серверов; хранятся последние 20 | `:dev`, `:X.Y.Z-dev.N` | pre-release на странице Releases |

Образы многоархитектурные (amd64 и arm64). Для **Raspberry Pi 3/4 и других процессоров
без аппаратного AES** (в `/proc/cpuinfo` нет флага `aes`) есть варианты с xray, собранным
для быстрого AES-GCM на таком железе: образы `:latest-noaes`, `:X.Y.Z-noaes`, `:dev-noaes`
(только arm64) и архивы с `-noaes` в имени (aarch64 и armv7).

Если на открытом issue висит метка `стоп-релиз`, продвижение из dev в stable останавливается.

### Проверка подлинности

Архивы и образы подписаны аттестациями происхождения сборки (GitHub), образы ещё и SBOM.
Проверка нужна [GitHub CLI](https://cli.github.com/):

```sh
sha256sum -c SHA256SUMS
gh attestation verify raycat-X.Y.Z-x86_64-linux-musl.tar.gz --repo raycat-app/raycat
gh attestation verify oci://ghcr.io/raycat-app/raycat:latest --repo raycat-app/raycat
```

## Участие

Предложения и исправления приветствуются: см. [CONTRIBUTING.md](CONTRIBUTING.md).
Об уязвимостях сообщайте приватно: [SECURITY.md](SECURITY.md).

## Лицензия

[MIT](LICENSE).
