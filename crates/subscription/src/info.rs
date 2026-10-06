use std::time::Duration;

use serde::Serialize;

use crate::routing::{self, Routing};
use crate::text::{clean, decode_header_text, percent_decode, push_warning};

const MAX_TITLE: usize = 200;
const MAX_ANNOUNCE: usize = 2000;
const MAX_URL: usize = 2048;
const MAX_INTERVAL_HOURS: u64 = 8760;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub upload: u64,
    pub download: u64,
    /// 0 — без ограничения.
    pub total: u64,
    /// Unix-время окончания; 0 — бессрочно.
    pub expire: u64,
}

impl Usage {
    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }
}

/// Признаки лимита устройств из ответа Remnawave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct HwidFlags {
    /// `x-hwid-active`: панель проверяет лимит устройств для пользователя.
    pub active: bool,
    /// `x-hwid-not-supported`: панель не получила корректный `x-hwid`.
    pub not_supported: bool,
    /// `x-hwid-max-devices-reached`: HWID новый, а все места заняты.
    pub max_devices_reached: bool,
    /// `x-hwid-limit`: общий признак отказа (совместимость с `v2RayTun`).
    pub limit: bool,
}

/// Сведения провайдера из заголовков ответа (и строк `#имя: значение` в теле).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderInfo {
    pub title: Option<String>,
    pub usage: Option<Usage>,
    pub update_interval: Option<Duration>,
    pub support_url: Option<String>,
    pub web_page_url: Option<String>,
    pub announce: Option<String>,
    /// Unix-время следующего сброса трафика.
    pub refill_date: Option<u64>,
    pub hwid: HwidFlags,
    pub routing: Option<Routing>,
    /// Запасная ссылка подписки (`fallback-url`).
    pub fallback_url: Option<String>,
    /// Провайдер переехал на другой домен: путь остаётся прежним.
    pub new_domain: Option<String>,
    /// Провайдер переехал: полная новая ссылка подписки.
    pub new_url: Option<String>,
    pub change_user_agent: Option<String>,
    /// `sub-expire`: провайдер просит напоминать об окончании подписки.
    pub sub_expire: bool,
}

/// Заголовки ответа и заголовки из тела; при совпадении имён побеждает HTTP.
struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    fn new(http: &[(String, String)], body: &[(String, String)]) -> Self {
        let entries = http
            .iter()
            .chain(body)
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        Self { entries }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.as_str())
    }

    fn flag(&self, name: &str) -> bool {
        self.get(name)
            .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
    }

    /// Текстовое поле с необязательным `base64:`.
    fn text(&self, name: &str, max: usize) -> Option<String> {
        let value = clean(&decode_header_text(self.get(name)?), max);
        (!value.is_empty()).then_some(value)
    }

    fn url(&self, name: &str) -> Option<String> {
        let value = clean(self.get(name)?, MAX_URL);
        (!value.is_empty()).then_some(value)
    }
}

/// Собирает сведения провайдера. `body_routing` — строка `happ://routing/…` из
/// тела ответа, она нужна, только если заголовка `routing` нет.
pub(crate) fn parse(
    http: &[(String, String)],
    body: &[(String, String)],
    body_routing: Option<&str>,
) -> (ProviderInfo, Vec<String>) {
    let headers = Headers::new(http, body);
    let mut warnings = Vec::new();
    let routing =
        headers
            .get("routing")
            .or(body_routing)
            .and_then(|value| match routing::parse(value) {
                Ok(routing) => Some(routing),
                Err(reason) => {
                    push_warning(
                        &mut warnings,
                        format!("профиль маршрутизации отброшен: {reason}"),
                    );
                    None
                }
            });
    let title = headers.text("profile-title", MAX_TITLE).or_else(|| {
        let name = clean(
            &disposition_filename(headers.get("content-disposition")?)?,
            MAX_TITLE,
        );
        (!name.is_empty()).then_some(name)
    });
    let info = ProviderInfo {
        title,
        usage: headers
            .get("subscription-userinfo")
            .and_then(parse_userinfo),
        update_interval: headers
            .get("profile-update-interval")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|hours| *hours > 0)
            .map(|hours| Duration::from_secs(hours.min(MAX_INTERVAL_HOURS) * 3600)),
        support_url: headers.url("support-url"),
        web_page_url: headers.url("profile-web-page-url"),
        announce: headers.text("announce", MAX_ANNOUNCE),
        refill_date: headers
            .get("subscription-refill-date")
            .and_then(|value| value.parse().ok()),
        hwid: HwidFlags {
            active: headers.flag("x-hwid-active"),
            not_supported: headers.flag("x-hwid-not-supported"),
            max_devices_reached: headers.flag("x-hwid-max-devices-reached"),
            limit: headers.flag("x-hwid-limit"),
        },
        routing,
        fallback_url: headers.url("fallback-url"),
        new_domain: headers.url("new-domain"),
        new_url: headers.url("new-url"),
        change_user_agent: headers.text("change-user-agent", MAX_URL),
        sub_expire: headers.flag("sub-expire"),
    };
    (info, warnings)
}

/// `upload=0; download=123; total=456; expire=1767225600` (порядок и пробелы разные).
fn parse_userinfo(value: &str) -> Option<Usage> {
    let mut usage = Usage::default();
    let mut found = false;
    for part in value.split(';') {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        // Некоторые панели присылают дробные числа ("123.0") или пустые значения.
        let number = raw
            .trim()
            .split('.')
            .next()
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(0);
        let slot = match key.trim().to_ascii_lowercase().as_str() {
            "upload" => &mut usage.upload,
            "download" => &mut usage.download,
            "total" => &mut usage.total,
            "expire" => &mut usage.expire,
            _ => continue,
        };
        *slot = number;
        found = true;
    }
    found.then_some(usage)
}

/// `attachment; filename*=UTF-8''%D0%9C%D0%BE%D0%B9` или `attachment; filename="name"`.
fn disposition_filename(value: &str) -> Option<String> {
    const EXTENDED: &str = "filename*=";
    const PLAIN: &str = "filename=";
    let lower = value.to_ascii_lowercase();
    if let Some(index) = lower.find(EXTENDED) {
        let rest = value.get(index + EXTENDED.len()..)?;
        let encoded = rest.split(';').next()?.trim();
        let encoded = encoded.rsplit("''").next()?;
        let name = percent_decode(encoded);
        return (!name.is_empty()).then_some(name);
    }
    let index = lower.find(PLAIN)?;
    let rest = value.get(index + PLAIN.len()..)?;
    let name = rest.split(';').next()?.trim().trim_matches('"');
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn info(items: &[(&str, &str)]) -> ProviderInfo {
        parse(&pairs(items), &[], None).0
    }

    #[test]
    fn provider_headers() {
        let info = info(&[
            (
                "Subscription-Userinfo",
                "upload=0; download=1073741824; total=107374182400; expire=1767225600",
            ),
            ("Profile-Title", "base64:0JzQvtC5IFZQTg=="),
            ("profile-update-interval", "12"),
            ("support-url", "https://example.com/support"),
            ("profile-web-page-url", "https://example.com/cabinet"),
            ("announce", "base64:0J/RgNC40LLQtdGC"),
            ("subscription-refill-date", "1767225600"),
            ("x-hwid-active", "true"),
            ("fallback-url", "https://reserve.example.com/sub"),
            ("new-domain", "moved.example.com"),
            ("new-url", "https://moved.example.com/sub"),
            ("change-user-agent", "Example/1.0"),
            ("sub-expire", "1"),
        ]);
        let usage = info.usage.unwrap();
        assert_eq!(usage.used(), 1_073_741_824);
        assert_eq!(usage.total, 107_374_182_400);
        assert_eq!(usage.expire, 1_767_225_600);
        assert_eq!(info.title.as_deref(), Some("Мой VPN"));
        assert_eq!(info.announce.as_deref(), Some("Привет"));
        assert_eq!(info.update_interval, Some(Duration::from_secs(12 * 3600)));
        assert_eq!(
            info.support_url.as_deref(),
            Some("https://example.com/support")
        );
        assert_eq!(
            info.web_page_url.as_deref(),
            Some("https://example.com/cabinet")
        );
        assert_eq!(info.refill_date, Some(1_767_225_600));
        assert!(info.hwid.active && !info.hwid.max_devices_reached);
        assert_eq!(
            info.fallback_url.as_deref(),
            Some("https://reserve.example.com/sub")
        );
        assert_eq!(info.new_domain.as_deref(), Some("moved.example.com"));
        assert_eq!(
            info.new_url.as_deref(),
            Some("https://moved.example.com/sub")
        );
        assert_eq!(info.change_user_agent.as_deref(), Some("Example/1.0"));
        assert!(info.sub_expire);
        assert!(info.routing.is_none());
    }

    #[test]
    fn hwid_flags() {
        let info = info(&[
            ("X-HWID-Not-Supported", "true"),
            ("x-hwid-max-devices-reached", "TRUE"),
            ("x-hwid-limit", "1"),
            ("x-hwid-active", "false"),
        ]);
        assert!(info.hwid.not_supported && info.hwid.max_devices_reached && info.hwid.limit);
        assert!(!info.hwid.active);
    }

    #[test]
    fn interval_is_capped_and_positive() {
        let huge = info(&[("profile-update-interval", "99999999999999")]);
        assert_eq!(huge.update_interval, Some(Duration::from_secs(8760 * 3600)));
        assert_eq!(
            info(&[("profile-update-interval", "0")]).update_interval,
            None
        );
        assert_eq!(
            info(&[("profile-update-interval", "abc")]).update_interval,
            None
        );
    }

    #[test]
    fn announce_line_breaks_do_not_glue_words() {
        let info = info(&[(
            "announce",
            "Продлите подписку\nЕсли баланс низкий\r\n\r\nпополните",
        )]);
        assert_eq!(
            info.announce.as_deref(),
            Some("Продлите подписку Если баланс низкий пополните")
        );
    }

    #[test]
    fn control_characters_are_stripped() {
        let info = info(&[
            ("profile-title", "VPN\u{1b}[31m\r\nred"),
            ("announce", "base64:0J/RgNC40LLQtdGCCg=="),
            ("support-url", "https://example.com/\u{7}help"),
        ]);
        assert_eq!(info.title.as_deref(), Some("VPN[31m red"));
        assert_eq!(info.announce.as_deref(), Some("Привет"));
        assert_eq!(
            info.support_url.as_deref(),
            Some("https://example.com/help")
        );
    }

    #[test]
    fn headers_from_body_lose_to_http() {
        let http = pairs(&[("Profile-Title", "Из заголовка")]);
        let body = pairs(&[
            ("profile-title", "Из тела"),
            ("support-url", "https://example.com/from-body"),
            ("profile-update-interval", "6"),
        ]);
        let (info, warnings) = parse(&http, &body, None);
        assert!(warnings.is_empty());
        assert_eq!(info.title.as_deref(), Some("Из заголовка"));
        assert_eq!(
            info.support_url.as_deref(),
            Some("https://example.com/from-body")
        );
        assert_eq!(info.update_interval, Some(Duration::from_secs(6 * 3600)));
    }

    #[test]
    fn empty_http_value_falls_back_to_body() {
        let http = pairs(&[("profile-title", "  ")]);
        let body = pairs(&[("profile-title", "Из тела")]);
        assert_eq!(
            parse(&http, &body, None).0.title.as_deref(),
            Some("Из тела")
        );
    }

    #[test]
    fn routing_header_and_body_line() {
        let (info, warnings) = parse(&pairs(&[("routing", "happ://routing/off")]), &[], None);
        assert_eq!(info.routing, Some(Routing::Off));
        assert!(warnings.is_empty());

        let (info, _) = parse(&[], &pairs(&[("routing", "happ://routing/off")]), None);
        assert_eq!(info.routing, Some(Routing::Off));

        let (info, _) = parse(&[], &[], Some("happ://routing/off"));
        assert_eq!(info.routing, Some(Routing::Off));

        let (info, _) = parse(
            &pairs(&[("routing", "happ://routing/off")]),
            &[],
            Some("happ://routing/add/e30="),
        );
        assert_eq!(info.routing, Some(Routing::Off));
    }

    #[test]
    fn broken_routing_gives_a_warning() {
        let (info, warnings) = parse(&pairs(&[("routing", "happ://routing/add/!!!")]), &[], None);
        assert!(info.routing.is_none());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn userinfo_tolerates_messy_values() {
        let usage = parse_userinfo("upload=1.0;download=;total=5; expire=0;junk").unwrap();
        assert_eq!(
            usage,
            Usage {
                upload: 1,
                download: 0,
                total: 5,
                expire: 0
            }
        );
        assert!(parse_userinfo("nothing useful").is_none());
        let reordered = parse_userinfo("EXPIRE=9; Total=8").unwrap();
        assert_eq!((reordered.total, reordered.expire), (8, 9));
    }

    #[test]
    fn title_falls_back_to_content_disposition() {
        let info = info(&[(
            "content-disposition",
            "attachment; filename*=UTF-8''%D0%9C%D0%BE%D0%B9",
        )]);
        assert_eq!(info.title.as_deref(), Some("Мой"));
        let info = super::parse(
            &pairs(&[("Content-Disposition", "attachment; filename=\"user42\"")]),
            &[],
            None,
        )
        .0;
        assert_eq!(info.title.as_deref(), Some("user42"));
    }
}
