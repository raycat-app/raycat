use std::collections::BTreeSet;

use serde::Deserialize;

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(
        title = "Настройки raycat",
        description = "Файл настроек raycat в формате TOML.",
        extend("$id" = "https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/config.schema.json"),
    )
)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Raw {
    /// Что сообщается провайдеру об устройстве.
    pub(crate) device: RawDevice,
    /// Подписки (`[[subscription]]`): первая основная, остальные резервные.
    pub(crate) subscription: Vec<RawSubscription>,
    /// Выбор узла: проверки доступности, переключение, закрепление.
    pub(crate) selection: RawSelection,
    /// Режим работы: прокси или шлюз.
    pub(crate) mode: RawMode,
    /// DNS-серверы, которыми пользуется raycat.
    pub(crate) dns: RawDns,
    /// Маршрутизация, которую присылает провайдер.
    pub(crate) routing: RawRouting,
    /// Исполняемый файл и параметры xray.
    pub(crate) xray: RawXray,
    /// Журнал.
    pub(crate) log: RawLog,
    /// Переменные окружения, которые задали значения (например `RAYCAT_LAN`).
    #[cfg_attr(feature = "schema", schemars(skip))]
    #[serde(skip)]
    pub(crate) from_env: BTreeSet<&'static str>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawDevice {
    /// Слово, по которому провайдер узнаёт устройство на любом сервере. Не задано: raycat создаёт машинный идентификатор сам. Непустая строка до 256 символов; нельзя вместе с `machine_id`.
    pub(crate) seed: Option<String>,
    /// Идентификатор устройства из 32 шестнадцатеричных символов, если провайдер его уже знает. Не задано: raycat создаёт его сам.
    pub(crate) machine_id: Option<String>,
    /// Имя компьютера в заголовках запроса к провайдеру. Не задано: значение выбирает raycat. Непустая строка до 128 символов, без управляющих символов.
    pub(crate) hostname: Option<String>,
    /// Модель устройства в заголовках запроса к провайдеру. Не задано: значение выбирает raycat. Непустая строка до 128 символов, без управляющих символов.
    pub(crate) model: Option<String>,
    /// Производитель устройства в заголовках запроса к провайдеру. Не задано: значение выбирает raycat. Непустая строка до 128 символов, без управляющих символов.
    pub(crate) manufacturer: Option<String>,
    /// Версия операционной системы в заголовках запроса к провайдеру. Не задано: значение выбирает raycat. Непустая строка до 128 символов, без управляющих символов.
    pub(crate) os_version: Option<String>,
    /// Язык системы в заголовках запроса к провайдеру, например `ru`. Не задано: значение выбирает raycat. Непустая строка до 128 символов, без управляющих символов.
    pub(crate) locale: Option<String>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawSubscription {
    /// Имя подписки для команд и `selection.pin`: от 1 до 64 символов, без `/` и управляющих символов. Имена не повторяются. Обязательно.
    pub(crate) name: Option<String>,
    /// Ссылка подписки: `https://…` (`http://` только с `allow_http = true`), до 2048 символов, без логина и пароля. Обязательна, если не задан `url_file`. Это секрет.
    pub(crate) url: Option<String>,
    /// Путь к файлу с одной ссылкой (до 4 килобайт), например Docker secret. Вместе с `url` не задаётся.
    pub(crate) url_file: Option<String>,
    /// Разрешает `http://` для этой подписки. По умолчанию `false`: по http ссылка и ответ передаются открыто.
    pub(crate) allow_http: Option<bool>,
    /// Приложение, которое имитирует raycat: `happ` или `incy`. Обязательно.
    #[cfg_attr(feature = "schema", schemars(extend("enum" = ["happ", "incy"])))]
    pub(crate) app: Option<String>,
    /// Платформа, которую имитирует raycat: `windows` или `android`. Для `incy` допустим только `android`. Обязательно.
    #[cfg_attr(feature = "schema", schemars(extend("enum" = ["windows", "android"])))]
    pub(crate) platform: Option<String>,
    /// Собственное устройство для этой подписки вместо `device.seed`. Непустая строка до 256 символов.
    pub(crate) seed: Option<String>,
    /// Как часто обновлять подписку, например `12h`: от `10m` до `30d`. Не задано: интервал из ответа провайдера, а если его нет, `12h`.
    pub(crate) update_interval: Option<String>,
    /// Белый список масок имён узлов: `*` — любое число символов, `?` — один символ, регистр не важен. Пусто — все узлы. До 256 масок.
    pub(crate) allow: Vec<String>,
    /// Чёрный список масок: узлы, подходящие под маску, не используются. До 256 масок.
    pub(crate) deny: Vec<String>,
    /// Предпочтительные маски узлов по порядку: чем раньше маска, тем выше приоритет. До 256 масок.
    pub(crate) priority: Vec<String>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawSelection {
    /// Адрес для проверки доступности узлов: `https://` или `http://`. По умолчанию `https://www.gstatic.com/generate_204`.
    pub(crate) check_url: Option<String>,
    /// Как часто проверять узлы, например `30s`: от `5s` до `10m`. По умолчанию `30s`.
    pub(crate) check_interval: Option<String>,
    /// Сколько проверок подряд без ответа делают узел недоступным: целое от 1 до 20. По умолчанию 3.
    #[cfg_attr(feature = "schema", schemars(extend("minimum" = 1, "maximum" = 20)))]
    pub(crate) failures: Option<i64>,
    /// На сколько узел того же приоритета должен быть быстрее текущего, чтобы переключиться, например `150ms`: от `0ms` до `60s`. По умолчанию `150ms`.
    pub(crate) switch_gain: Option<String>,
    /// Сколько приоритетный узел должен непрерывно работать, прежде чем на него вернуться, например `5m`: от `0ms` до `24h`. По умолчанию `5m`.
    pub(crate) return_delay: Option<String>,
    /// Закреплённый узел в виде `имя подписки/имя узла`, например `основная/NL-1`. Подписка должна быть в настройках.
    pub(crate) pin: Option<String>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawMode {
    /// Режим работы: `proxy` — прокси для программ на сервере, `gateway` — шлюз, через который идёт весь трафик сервера. По умолчанию `proxy`.
    #[cfg_attr(feature = "schema", schemars(extend("enum" = ["proxy", "gateway"])))]
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    /// Адрес прокси в режиме `proxy`, например `127.0.0.1:7890`. По умолчанию `127.0.0.1:7890`; порт `0` недопустим.
    pub(crate) listen: Option<String>,
    /// Блокировать трафик, когда VPN недоступен, — только в режиме `gateway`. По умолчанию `true`.
    pub(crate) kill_switch: Option<bool>,
    /// Пропускать через шлюз устройства локальной сети — только в режиме `gateway`. По умолчанию `false`.
    pub(crate) lan: Option<bool>,
    /// Интерфейс, с которого приходят пакеты устройств сети, например `br-lan`. Не задано: интерфейс маршрута по умолчанию. Действует только при `lan = true`.
    pub(crate) lan_interface: Option<String>,
    /// Подсети устройств в виде CIDR, например `192.168.1.0/24`: от 1 до 32, только `IPv4`. Не задано: подсети интерфейса. Действует только при `lan = true`.
    pub(crate) lan_subnets: Option<Vec<String>>,
    pub(crate) auth: Option<String>,
    pub(crate) auth_file: Option<String>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawDns {
    /// IP-адреса DNS-серверов: от 1 до 8. По умолчанию `1.1.1.1` и `8.8.8.8`.
    pub(crate) resolvers: Option<Vec<String>>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawRouting {
    /// Применять маршрутизацию, которую присылает провайдер. По умолчанию `false`.
    pub(crate) provider: Option<bool>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawXray {
    /// Путь к исполняемому файлу xray. По умолчанию `/usr/libexec/raycat/xray`.
    pub(crate) path: Option<String>,
    /// Лимит памяти процесса xray, например `96MiB`: от `16MiB` до `16GiB`. По умолчанию `96MiB`. Единицы: `B`, `KiB`, `MiB`, `GiB`.
    pub(crate) memory_limit: Option<String>,
    /// Алгоритм управления перегрузкой TCP: `auto` (BBR, если хост это позволяет), `off` или имя алгоритма ядра, например `bbr` или `cubic`. До 15 символов: латиница, цифры, «-» и «_». По умолчанию `auto`.
    #[cfg_attr(feature = "schema", schemars(extend("pattern" = "^[A-Za-z0-9_-]{1,15}$")))]
    pub(crate) tcp_congestion: Option<String>,
    /// Число параллельных соединений XHTTP: целое от 1 до 16. Не задано: как у провайдера.
    #[cfg_attr(feature = "schema", schemars(extend("minimum" = 1, "maximum" = 16)))]
    pub(crate) xhttp_connections: Option<i64>,
}

#[derive(Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawLog {
    /// Уровень журнала: `error`, `warn`, `info` или `debug`. По умолчанию `info`.
    #[cfg_attr(feature = "schema", schemars(extend("enum" = ["error", "warn", "info", "debug"])))]
    pub(crate) level: Option<String>,
}
