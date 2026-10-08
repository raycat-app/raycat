//! Получение подписки: запрос как у приложения, перенаправления, запасные адреса,
//! проверка ответа и запись в кэш.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use jiff::tz::TimeZone;
use raycat_config::{Config, Platform as ConfigPlatform, Subscription};
use raycat_emulation::{Arch, Device, Emulation, Platform, machine_id_from_seed};
use raycat_http::{Client, Request, Response, Scheme, Url, redact};
use raycat_subscription::{Analysis, Problem, ProviderInfo, analyze, redact_in};
use raycat_xray::Node;

use crate::log::{self, Latch, Level, hide, info, warn};
use crate::plan;
use crate::schedule::{after_retry_header, interval, is_outage, rejected_delay, retry_delay};
use crate::store::{Store, SubState};
use crate::util::{fnv1a, format_bytes, format_date, now_unix};

const MAX_REDIRECTS: usize = 5;
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Откуда взят идентификатор эмулируемого устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    SubscriptionSeed,
    DeviceSeed,
    MachineId,
    StateFile,
}

impl Origin {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::SubscriptionSeed => "seed подписки",
            Self::DeviceSeed => "seed из раздела [device]",
            Self::MachineId => "machine_id из раздела [device]",
            Self::StateFile => "файл machine-id в каталоге состояния",
        }
    }
}

fn machine_id_for(config: &Config, subscription: &Subscription, stored: &str) -> (String, Origin) {
    if let Some(seed) = &subscription.seed {
        return (
            machine_id_from_seed(seed.expose()),
            Origin::SubscriptionSeed,
        );
    }
    if let Some(seed) = &config.device.seed {
        return (machine_id_from_seed(seed.expose()), Origin::DeviceSeed);
    }
    if let Some(id) = &config.device.machine_id {
        return (id.expose().to_owned(), Origin::MachineId);
    }
    (stored.to_owned(), Origin::StateFile)
}

fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
}

fn emulated_device(config: &Config, machine_id: String) -> Device {
    let device = &config.device;
    Device {
        machine_id,
        hostname: device.hostname.clone(),
        model: device.model.clone(),
        manufacturer: device.manufacturer.clone(),
        os_version: device.os_version.clone(),
        hwid: None,
        locale: device.locale.clone().unwrap_or_else(system_locale),
    }
}

fn emulated_platform(platform: ConfigPlatform) -> Platform {
    match platform {
        ConfigPlatform::Windows => Platform::Windows,
        ConfigPlatform::Android => Platform::Android,
    }
}

fn parse_url(text: &str) -> Option<Url> {
    Url::parse(text).ok()
}

/// Одна подписка из настроек вместе с её эмулируемым устройством и клиентом.
pub(crate) struct Source {
    config: Subscription,
    emulation: Emulation,
    client: Client,
    fingerprint: String,
    origin: Origin,
    /// Основной адрес не отвечает, но запасной подошёл: об этом пишется один раз.
    address_problem: Mutex<Latch>,
}

impl Source {
    pub(crate) fn new(
        config: &Config,
        subscription: &Subscription,
        stored_machine_id: &str,
    ) -> Result<Self> {
        let (machine_id, origin) = machine_id_for(config, subscription, stored_machine_id);
        let emulation = Emulation::new(
            subscription.app.as_str(),
            emulated_platform(subscription.platform),
            Arch::X64,
            &emulated_device(config, machine_id),
        )
        .with_context(|| {
            format!(
                "подписка «{}»: не удалось настроить эмуляцию",
                subscription.name
            )
        })?;
        hide(subscription.url.expose(), &subscription.masked_url());
        let fingerprint = format!(
            "{:016x}",
            fnv1a(
                format!(
                    "{}\0{}\0{}",
                    subscription.url.expose(),
                    subscription.app.as_str(),
                    subscription.platform.as_str()
                )
                .as_bytes()
            )
        );
        Ok(Self {
            config: subscription.clone(),
            emulation,
            client: Client {
                max_body: MAX_BODY,
                // В режиме шлюза kill switch выпускает наружу только помеченное.
                mark: plan::own_mark(config),
                ..Client::default()
            },
            fingerprint,
            origin,
            address_problem: Mutex::new(Latch::default()),
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.config.name
    }

    pub(crate) fn config(&self) -> &Subscription {
        &self.config
    }

    pub(crate) fn emulation(&self) -> &Emulation {
        &self.emulation
    }

    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }

    pub(crate) fn headers(&self, url: &Url) -> Vec<(String, String)> {
        self.emulation.headers(url, now_unix())
    }

    pub(crate) fn send_with(&self, url: &Url, headers: &[(String, String)]) -> Result<Response> {
        let request = Request {
            method: "GET",
            target: &url.target,
            headers,
            body: &[],
        };
        self.client
            .send(url, &request)
            .with_context(|| format!("запрос к {}", redact(url)))
    }

    pub(crate) fn send(&self, url: &Url) -> Result<Response> {
        self.send_with(url, &self.headers(url))
    }

    /// Состояние на диске; чужое (для другой ссылки или приложения) считается пустым.
    fn load(&self, store: &Store) -> (SubState, Option<Vec<u8>>) {
        let (state, body) = store.load(self.name());
        if state.source == self.fingerprint {
            (state, body)
        } else {
            let fresh = SubState {
                source: self.fingerprint.clone(),
                ..SubState::default()
            };
            (fresh, None)
        }
    }

    /// Узлы последнего рабочего ответа из кэша.
    pub(crate) fn cached(&self, store: &Store) -> Option<Cached> {
        let (state, body) = self.load(store);
        let analysis = analyze(state.status, &state.headers, &body?);
        if analysis.problem.is_some() {
            return None;
        }
        Some(Cached {
            fetched_at: state.fetched_at,
            nodes: analysis.nodes,
            info: analysis.info,
        })
    }

    /// Куда обращаться по порядку: адрес, на который переехал провайдер, адрес из
    /// настроек, запасной адрес. Запасной адрес по http не берётся, если остальные по https.
    fn candidates(&self, state: &SubState) -> Vec<Url> {
        let mut list = Vec::new();
        if let Some(url) = state.replaced_url.as_deref().and_then(parse_url) {
            list.push(url);
        }
        if let Some(url) = parse_url(self.config.url.expose())
            && !list.contains(&url)
        {
            list.push(url);
        }
        if let Some(url) = state.fallback_url.as_deref().and_then(parse_url)
            && !list.contains(&url)
            && !(url.scheme == Scheme::Http
                && list.iter().any(|known| known.scheme == Scheme::Https))
        {
            list.push(url);
        }
        list
    }
}

pub(crate) struct Cached {
    pub(crate) fetched_at: u64,
    pub(crate) nodes: Vec<Node>,
    pub(crate) info: ProviderInfo,
}

pub(crate) struct Fetched {
    pub(crate) response: Response,
    /// Адрес, с которого начался запрос, и адрес, который ответил после перенаправлений.
    pub(crate) requested: Url,
    pub(crate) served: Url,
}

fn redirect_target(response: &Response) -> Option<&str> {
    if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        response.header("location")
    } else {
        None
    }
}

/// Идёт по перенаправлениям (не больше пяти, с https на http не переходит).
pub(crate) fn follow(
    start: &Url,
    send: &mut dyn FnMut(&Url) -> Result<Response>,
) -> Result<(Response, Url)> {
    let mut url = start.clone();
    for _ in 0..=MAX_REDIRECTS {
        let response = send(&url)?;
        let Some(location) = redirect_target(&response) else {
            return Ok((response, url));
        };
        let next = url
            .join(location)
            .context("некорректный адрес в перенаправлении")?;
        if url.scheme == Scheme::Https && next.scheme == Scheme::Http {
            bail!("перенаправление с https на http отклонено");
        }
        url = next;
    }
    bail!("больше {MAX_REDIRECTS} перенаправлений подряд")
}

/// Пробует адреса по порядку до первого ответа 2xx; если такого нет, возвращает
/// последний исход. Отказ первого адреса — предупреждение, пока он не сменился
/// (`first_failed`); отказы остальных — только debug.
pub(crate) fn fetch_candidates(
    candidates: &[Url],
    send: &mut dyn FnMut(&Url) -> Result<Response>,
    first_failed: &mut Latch,
) -> Result<Fetched> {
    let mut last = None;
    for (index, requested) in candidates.iter().enumerate() {
        let attempt = follow(requested, send).map(|(response, served)| Fetched {
            response,
            requested: requested.clone(),
            served,
        });
        if matches!(&attempt, Ok(fetched) if (200..300).contains(&fetched.response.status)) {
            if index == 0 {
                first_failed.clear();
            }
            return attempt;
        }
        let reason = match &attempt {
            Ok(fetched) => format!("HTTP {}", fetched.response.status),
            Err(error) => format!("{error:#}"),
        };
        if index + 1 < candidates.len() {
            let text = format!(
                "адрес {} не подошёл ({reason}), пробую следующий",
                redact(requested)
            );
            let level = if index == 0 {
                first_failed.level(&text)
            } else {
                Level::Debug
            };
            log::write(level, format_args!("{text}"));
        }
        last = Some(attempt);
    }
    last.unwrap_or_else(|| Err(anyhow!("нет ни одного адреса подписки")))
}

/// Новый адрес подписки, если провайдер переехал. Переезд принимается только из
/// ответа по https и только на https: иначе перехватчик увёл бы подписку с токеном.
pub(crate) fn moved_to(info: &ProviderInfo, requested: &Url, served: &Url) -> Option<Url> {
    if served.scheme != Scheme::Https {
        return None;
    }
    let target = if let Some(full) = &info.new_url {
        Url::parse(full).ok()?
    } else if let Some(domain) = &info.new_domain {
        with_host(requested, domain)?
    } else {
        return None;
    };
    (target.scheme == Scheme::Https && target != *requested).then_some(target)
}

fn with_host(url: &Url, domain: &str) -> Option<Url> {
    let domain = domain.trim();
    let unfit = |c: char| matches!(c, '/' | '?' | '#' | '@') || c.is_whitespace() || c.is_control();
    if domain.is_empty() || domain.contains(unfit) {
        return None;
    }
    Url::parse(&format!("https://{domain}{}", url.target)).ok()
}

pub(crate) enum Outcome {
    /// Рабочий ответ: узлы можно применять.
    Applied(Box<Analysis>),
    /// Провайдер ответил по существу, но применять ответ нельзя (заглушка, отказ
    /// устройству, 4xx); кэш прежний. Повтор по обычному интервалу подписки.
    Rejected(String),
    /// Ответа нет: сбой связи или ошибка панели (5xx). Повтор с нарастающей паузой.
    Failed(String),
}

pub(crate) struct Refresh {
    pub(crate) outcome: Outcome,
    /// Через сколько обновлять снова.
    pub(crate) next_in: Duration,
}

/// Одно обновление подписки вместе с записью в кэш. Блокирует поток: вызывать из
/// `spawn_blocking`. `failures` — сколько сбоев ([`Outcome::Failed`]) подряд было до этого.
pub(crate) fn refresh(source: &Source, store: &Store, failures: u32, now: u64) -> Refresh {
    let (mut state, cached) = source.load(store);
    let candidates = source.candidates(&state);
    let mut send = |url: &Url| source.send(url);
    let fetched = {
        let mut latch = source
            .address_problem
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        fetch_candidates(&candidates, &mut send, &mut latch)
    };
    let (outcome, next_in) = match fetched {
        Err(error) => reject(
            &mut state,
            Outcome::Failed,
            format!("{error:#}"),
            retry_delay(failures.saturating_add(1), cached.is_some()),
        ),
        Ok(fetched) => process(
            source,
            store,
            &mut state,
            cached.as_deref(),
            &fetched,
            now,
            failures,
        ),
    };
    state.next_update = now.saturating_add(next_in.as_secs());
    if let Err(error) = store.save_state(source.name(), &state) {
        warn!(
            "подписка «{}»: не удалось сохранить состояние: {error:#}",
            source.name()
        );
    }
    Refresh { outcome, next_in }
}

fn reject(
    state: &mut SubState,
    kind: fn(String) -> Outcome,
    message: String,
    delay: Duration,
) -> (Outcome, Duration) {
    state.last_error = Some(message.clone());
    (kind(message), delay)
}

/// `profile-update-interval` последнего рабочего ответа из кэша подписки.
fn last_known_interval(state: &SubState, cached: Option<&[u8]>) -> Option<Duration> {
    analyze(state.status, &state.headers, cached?)
        .info
        .update_interval
}

fn process(
    source: &Source,
    store: &Store,
    state: &mut SubState,
    cached: Option<&[u8]>,
    fetched: &Fetched,
    now: u64,
    failures: u32,
) -> (Outcome, Duration) {
    let response = &fetched.response;
    let analysis = analyze(response.status, &response.headers, &response.body);
    if let Some(problem) = &analysis.problem {
        let message = redact_in(problem.message(), &[source.config.url.expose()]);
        let retry_after = response.header("retry-after");
        if matches!(problem, Problem::Http(_)) && is_outage(response.status) {
            let backoff = retry_delay(failures.saturating_add(1), cached.is_some());
            let delay = after_retry_header(backoff, retry_after);
            return reject(state, Outcome::Failed, message, delay);
        }
        let provider = analysis
            .info
            .update_interval
            .or_else(|| last_known_interval(state, cached));
        let regular = interval(source.config.update_interval, provider);
        return reject(
            state,
            Outcome::Rejected,
            message,
            rejected_delay(regular, retry_after),
        );
    }
    let body = response.body.as_slice();
    let saved = cached == Some(body)
        || store
            .save_body(source.name(), body)
            .inspect_err(|error| {
                warn!(
                    "подписка «{}»: не удалось сохранить ответ: {error:#}",
                    source.name()
                );
            })
            .is_ok();
    if saved {
        state.status = response.status;
        state.headers.clone_from(&response.headers);
        state.fetched_at = now;
    }
    state.last_error = None;
    remember_addresses(source.name(), state, &analysis.info, fetched);
    let next_in = interval(source.config.update_interval, analysis.info.update_interval);
    (Outcome::Applied(Box::new(analysis)), next_in)
}

/// Запоминает адреса из ответа: переезд провайдера и запасной адрес.
fn remember_addresses(name: &str, state: &mut SubState, info: &ProviderInfo, fetched: &Fetched) {
    if let Some(url) = moved_to(info, &fetched.requested, &fetched.served) {
        let text = url.to_string();
        if state.replaced_url.as_deref() != Some(text.as_str()) {
            hide(&text, &redact(&url));
            info!(
                "подписка «{name}»: провайдер переехал, новый адрес {}",
                redact(&url)
            );
            state.replaced_url = Some(text);
        }
    }
    if let Some(fallback) = info.fallback_url.as_deref().and_then(parse_url) {
        let downgrade =
            fetched.requested.scheme == Scheme::Https && fallback.scheme == Scheme::Http;
        if !downgrade {
            let text = fallback.to_string();
            hide(&text, &redact(&fallback));
            state.fallback_url = Some(text);
        }
    }
}

/// Строка для лога: название, число узлов, трафик и срок.
pub(crate) fn summary(info: &ProviderInfo, nodes: usize, zone: &TimeZone) -> String {
    let mut parts = Vec::new();
    if let Some(title) = &info.title {
        parts.push(title.clone());
    }
    parts.push(format!("узлов: {nodes}"));
    if let Some(usage) = &info.usage {
        let used = format_bytes(usage.used());
        parts.push(if usage.total == 0 {
            format!("трафик: {used} (без лимита)")
        } else {
            format!("трафик: {used} из {}", format_bytes(usage.total))
        });
        parts.push(if usage.expire == 0 {
            "срок не ограничен".to_owned()
        } else {
            format!("срок до {}", format_date(usage.expire, zone))
        });
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use raycat_config::Env;

    use super::*;
    use crate::testing::{FakePanel, TempDir, http_response};

    const MACHINE_ID: &str = "0d0af05ee8fd4dc29275718f2ce4dff1";
    const LINKS: &str = "ss://aes-128-gcm:secret@203.0.113.5:8388#One\nss://aes-128-gcm:secret@203.0.113.6:8388#Two\n";

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            reason: String::new(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
            peer: None,
        }
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn config_for(link: &str, extra: &str) -> Config {
        let text = format!(
            "[[subscription]]\nname = \"тест\"\nurl = \"{link}\"\nallow_http = true\napp = \"happ\"\nplatform = \"windows\"\n{extra}"
        );
        Config::from_toml_str(&text, &Env::new()).unwrap()
    }

    fn source_for(link: &str) -> (Config, Source) {
        let config = config_for(link, "");
        let source = Source::new(&config, &config.subscriptions[0], MACHINE_ID).unwrap();
        (config, source)
    }

    fn store(temp: &TempDir) -> Store {
        Store::open(temp.path().to_path_buf()).unwrap()
    }

    #[test]
    fn relative_redirects_are_followed() {
        let mut visited = Vec::new();
        let mut send = |url: &Url| -> Result<Response> {
            visited.push(url.to_string());
            Ok(if url.target == "/x/start" {
                response(302, &[("Location", "../final?x=1")], "")
            } else {
                response(200, &[], "ok")
            })
        };
        let (response, served) = follow(&url("https://a.example.com/x/start"), &mut send).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(served, url("https://a.example.com/final?x=1"));
        assert_eq!(
            visited,
            [
                "https://a.example.com/x/start",
                "https://a.example.com/final?x=1"
            ]
        );
    }

    #[test]
    fn absolute_and_protocol_relative_locations_work() {
        let mut send = |url: &Url| -> Result<Response> {
            Ok(match url.host.as_str() {
                "a.example.com" => response(301, &[("Location", "//b.example.com/next")], ""),
                "b.example.com" => response(307, &[("Location", "https://c.example.com/end")], ""),
                _ => response(200, &[], "ok"),
            })
        };
        let (_, served) = follow(&url("https://a.example.com/start"), &mut send).unwrap();
        assert_eq!(served, url("https://c.example.com/end"));
    }

    #[test]
    fn https_to_http_redirect_is_refused() {
        let mut send = |_: &Url| -> Result<Response> {
            Ok(response(302, &[("Location", "http://a.example.com/x")], ""))
        };
        let error = follow(&url("https://a.example.com/start"), &mut send).unwrap_err();
        assert!(error.to_string().contains("с https на http"), "{error}");
    }

    #[test]
    fn http_to_https_redirect_is_allowed() {
        let mut send = |url: &Url| -> Result<Response> {
            Ok(if url.scheme == Scheme::Http {
                response(302, &[("Location", "https://a.example.com/x")], "")
            } else {
                response(200, &[], "ok")
            })
        };
        let (response, served) = follow(&url("http://a.example.com/start"), &mut send).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(served.scheme, Scheme::Https);
    }

    #[test]
    fn at_most_five_redirects_are_followed() {
        let mut calls = 0;
        let mut send = |_: &Url| -> Result<Response> {
            calls += 1;
            Ok(response(302, &[("Location", "/again")], ""))
        };
        let error = follow(&url("https://a.example.com/start"), &mut send).unwrap_err();
        assert!(error.to_string().contains("перенаправлений"), "{error}");
        assert_eq!(calls, 6);
    }

    #[test]
    fn five_redirects_still_reach_the_answer() {
        let mut calls = 0;
        let mut send = |_: &Url| -> Result<Response> {
            calls += 1;
            Ok(if calls <= 5 {
                response(302, &[("Location", "/again")], "")
            } else {
                response(200, &[], "ok")
            })
        };
        let (response, _) = follow(&url("https://a.example.com/start"), &mut send).unwrap();
        assert_eq!(response.status, 200);
    }

    #[test]
    fn a_redirect_without_location_is_the_answer() {
        let mut send = |_: &Url| -> Result<Response> { Ok(response(302, &[], "")) };
        let (response, _) = follow(&url("https://a.example.com/start"), &mut send).unwrap();
        assert_eq!(response.status, 302);
    }

    #[test]
    fn a_bad_location_is_an_error() {
        let mut send = |_: &Url| -> Result<Response> {
            Ok(response(302, &[("Location", "ftp://a.example.com/x")], ""))
        };
        assert!(follow(&url("https://a.example.com/start"), &mut send).is_err());
    }

    #[test]
    fn fallback_is_used_after_a_network_error() {
        let mut send = |url: &Url| -> Result<Response> {
            if url.host == "main.example.com" {
                Err(anyhow!("нет связи"))
            } else {
                Ok(response(200, &[], "ok"))
            }
        };
        let candidates = [
            url("https://main.example.com/sub/aaaa"),
            url("https://reserve.example.com/sub/bbbb"),
        ];
        let fetched = fetch_candidates(&candidates, &mut send, &mut Latch::default()).unwrap();
        assert_eq!(fetched.requested, candidates[1]);
        assert_eq!(fetched.response.status, 200);
    }

    #[test]
    fn fallback_is_used_after_a_bad_status() {
        let mut send = |url: &Url| -> Result<Response> {
            Ok(if url.host == "main.example.com" {
                response(404, &[], "")
            } else {
                response(200, &[], "ok")
            })
        };
        let candidates = [
            url("https://main.example.com/sub/aaaa"),
            url("https://reserve.example.com/sub/bbbb"),
        ];
        let fetched = fetch_candidates(&candidates, &mut send, &mut Latch::default()).unwrap();
        assert_eq!(fetched.requested.host, "reserve.example.com");
    }

    #[test]
    fn a_good_first_address_ends_the_search() {
        let mut calls = Vec::new();
        let mut send = |url: &Url| -> Result<Response> {
            calls.push(url.host.clone());
            Ok(response(200, &[], "ok"))
        };
        let candidates = [
            url("https://main.example.com/sub/aaaa"),
            url("https://reserve.example.com/sub/bbbb"),
        ];
        fetch_candidates(&candidates, &mut send, &mut Latch::default()).unwrap();
        assert_eq!(calls, ["main.example.com"]);
    }

    #[test]
    fn when_every_address_fails_the_last_outcome_is_returned() {
        let mut send = |url: &Url| -> Result<Response> {
            if url.host == "main.example.com" {
                Ok(response(500, &[], ""))
            } else {
                Err(anyhow!("нет связи"))
            }
        };
        let candidates = [
            url("https://main.example.com/sub/aaaa"),
            url("https://reserve.example.com/sub/bbbb"),
        ];
        let mut latch = Latch::default();
        let error = fetch_candidates(&candidates, &mut send, &mut latch)
            .err()
            .unwrap();
        assert!(error.to_string().contains("нет связи"));
        assert!(fetch_candidates(&[], &mut send, &mut latch).is_err());
    }

    #[test]
    fn a_dead_first_address_is_remembered_until_it_answers_again() {
        let mut send = |url: &Url| -> Result<Response> {
            if url.host == "main.example.com" {
                Err(anyhow!("нет связи"))
            } else {
                Ok(response(200, &[], "ok"))
            }
        };
        let candidates = [
            url("https://main.example.com/sub/aaaa"),
            url("https://reserve.example.com/sub/bbbb"),
        ];
        let mut latch = Latch::default();
        fetch_candidates(&candidates, &mut send, &mut latch).unwrap();
        assert!(latch.clear());
        fetch_candidates(&candidates, &mut send, &mut latch).unwrap();
        fetch_candidates(&candidates, &mut send, &mut latch).unwrap();
        assert!(latch.clear());

        fetch_candidates(&candidates, &mut send, &mut latch).unwrap();
        let mut working = |_: &Url| -> Result<Response> { Ok(response(200, &[], "ok")) };
        fetch_candidates(&candidates, &mut working, &mut latch).unwrap();
        assert!(!latch.clear());
    }

    #[test]
    fn candidates_go_replaced_configured_fallback() {
        let (_, source) = source_for("https://main.example.com/sub/aaaa");
        let state = SubState {
            replaced_url: Some("https://moved.example.com/sub/aaaa".to_owned()),
            fallback_url: Some("https://reserve.example.com/sub/bbbb".to_owned()),
            ..SubState::default()
        };
        let hosts: Vec<String> = source
            .candidates(&state)
            .into_iter()
            .map(|url| url.host)
            .collect();
        assert_eq!(
            hosts,
            [
                "moved.example.com",
                "main.example.com",
                "reserve.example.com"
            ]
        );
        let plain: Vec<String> = source
            .candidates(&SubState::default())
            .into_iter()
            .map(|url| url.host)
            .collect();
        assert_eq!(plain, ["main.example.com"]);
    }

    #[test]
    fn duplicate_candidates_are_dropped() {
        let (_, source) = source_for("https://main.example.com/sub/aaaa");
        let state = SubState {
            fallback_url: Some("https://main.example.com/sub/aaaa".to_owned()),
            ..SubState::default()
        };
        assert_eq!(source.candidates(&state).len(), 1);
    }

    #[test]
    fn an_http_fallback_never_follows_https_addresses() {
        let (_, source) = source_for("https://main.example.com/sub/aaaa");
        let state = SubState {
            fallback_url: Some("http://reserve.example.com/sub/bbbb".to_owned()),
            ..SubState::default()
        };
        assert_eq!(source.candidates(&state).len(), 1);
    }

    fn moved(new_url: Option<&str>, new_domain: Option<&str>) -> ProviderInfo {
        ProviderInfo {
            new_url: new_url.map(str::to_owned),
            new_domain: new_domain.map(str::to_owned),
            ..ProviderInfo::default()
        }
    }

    #[test]
    fn new_domain_keeps_the_path_and_query() {
        let requested = url("https://old.example.com/sub/abcd?x=1");
        let target = moved_to(
            &moved(None, Some("new.example.com")),
            &requested,
            &requested,
        );
        assert_eq!(target, Some(url("https://new.example.com/sub/abcd?x=1")));
    }

    #[test]
    fn new_url_replaces_the_whole_address() {
        let requested = url("https://old.example.com/sub/abcd");
        let info = moved(Some("https://moved.example.com/other/efgh"), None);
        assert_eq!(
            moved_to(&info, &requested, &requested),
            Some(url("https://moved.example.com/other/efgh"))
        );
    }

    #[test]
    fn a_move_from_an_http_answer_is_ignored() {
        let requested = url("http://old.example.com/sub/abcd");
        let info = moved(None, Some("new.example.com"));
        assert_eq!(moved_to(&info, &requested, &requested), None);
    }

    #[test]
    fn a_move_to_http_is_ignored() {
        let requested = url("https://old.example.com/sub/abcd");
        let info = moved(Some("http://moved.example.com/sub/abcd"), None);
        assert_eq!(moved_to(&info, &requested, &requested), None);
    }

    #[test]
    fn a_move_to_the_same_address_or_nowhere_is_ignored() {
        let requested = url("https://old.example.com/sub/abcd");
        assert_eq!(
            moved_to(
                &moved(None, Some("old.example.com")),
                &requested,
                &requested
            ),
            None
        );
        assert_eq!(moved_to(&moved(None, None), &requested, &requested), None);
    }

    #[test]
    fn a_hostile_domain_cannot_smuggle_a_path_or_credentials() {
        let requested = url("https://old.example.com/sub/abcd");
        for domain in [
            "evil.example.com/x",
            "user@evil.example.com",
            "a b",
            "a?b",
            "",
        ] {
            let info = moved(None, Some(domain));
            assert_eq!(moved_to(&info, &requested, &requested), None, "{domain:?}");
        }
    }

    #[test]
    fn summary_lists_title_nodes_traffic_and_expiry() {
        let mut info = ProviderInfo::default();
        assert_eq!(summary(&info, 3, &TimeZone::UTC), "узлов: 3");
        info.title = Some("Тест".to_owned());
        info.usage = Some(raycat_subscription::Usage {
            upload: 1,
            download: 2,
            total: 100,
            expire: 0,
        });
        assert_eq!(
            summary(&info, 2, &TimeZone::UTC),
            "Тест, узлов: 2, трафик: 3 Б из 100 Б, срок не ограничен"
        );
        if let Some(usage) = &mut info.usage {
            usage.total = 0;
            usage.expire = 1_790_596_800;
        }
        assert_eq!(
            summary(&info, 2, &TimeZone::UTC),
            "Тест, узлов: 2, трафик: 3 Б (без лимита), срок до 28.09.2026"
        );
    }

    #[test]
    fn the_device_comes_from_the_most_specific_seed() {
        let text = |sub_extra: &str, device: &str| {
            format!(
                "{device}\n[[subscription]]\nname = \"a\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n{sub_extra}"
            )
        };
        let load = |text: &str| Config::from_toml_str(text, &Env::new()).unwrap();

        let plain = load(&text("", ""));
        assert_eq!(
            machine_id_for(&plain, &plain.subscriptions[0], "stored").1,
            Origin::StateFile
        );
        let id = format!("{:032x}", 1);
        let device_id = load(&text("", &format!("[device]\nmachine_id = \"{id}\"")));
        assert_eq!(
            machine_id_for(&device_id, &device_id.subscriptions[0], "stored"),
            (id, Origin::MachineId)
        );
        let device_seed = load(&text("", "[device]\nseed = \"общий\""));
        let with_seed = load(&text("seed = \"свой\"", "[device]\nseed = \"общий\""));
        let (shared, origin) =
            machine_id_for(&device_seed, &device_seed.subscriptions[0], "stored");
        assert_eq!(origin, Origin::DeviceSeed);
        let (own, origin) = machine_id_for(&with_seed, &with_seed.subscriptions[0], "stored");
        assert_eq!(origin, Origin::SubscriptionSeed);
        assert_ne!(shared, own);
        assert_eq!(shared, machine_id_from_seed("общий"));
    }

    #[test]
    fn refresh_applies_and_caches_the_answer() {
        let panel = FakePanel::start(|_| {
            http_response(
                "200 OK",
                &[(
                    "Subscription-Userinfo",
                    "upload=1; download=2; total=100; expire=0",
                )],
                LINKS,
            )
        });
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("refresh");
        let store = store(&temp);

        let done = refresh(&source, &store, 0, 1_000);
        let Outcome::Applied(analysis) = done.outcome else {
            panic!("ответ не применён");
        };
        assert_eq!(analysis.nodes.len(), 2);
        assert_eq!(done.next_in, Duration::from_secs(12 * 3_600));

        let cached = source.cached(&store).unwrap();
        assert_eq!((cached.fetched_at, cached.nodes.len()), (1_000, 2));
        let (state, body) = store.load("тест");
        assert_eq!(state.next_update, 1_000 + 12 * 3_600);
        assert!(state.last_error.is_none());
        assert_eq!(body.unwrap(), LINKS.as_bytes());

        let requests = panel.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /sub/token1234 HTTP/1.1\r\n"));
        assert!(requests[0].contains("\r\nUser-Agent: Happ/"));
        assert!(requests[0].contains("\r\nX-Hwid: "));
    }

    #[test]
    fn a_bad_answer_keeps_the_last_working_one() {
        let calls = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&calls);
        let panel = FakePanel::start(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                http_response("200 OK", &[], LINKS)
            } else {
                http_response("500 Internal Server Error", &[], "boom")
            }
        });
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("keep");
        let store = store(&temp);
        assert!(matches!(
            refresh(&source, &store, 0, 1_000).outcome,
            Outcome::Applied(_)
        ));

        let second = refresh(&source, &store, 0, 2_000);
        let Outcome::Failed(reason) = second.outcome else {
            panic!("ошибка панели должна быть сбоем");
        };
        assert!(reason.contains("500"), "{reason}");
        assert_eq!(second.next_in, retry_delay(1, true));

        let (state, body) = store.load("тест");
        assert_eq!(state.fetched_at, 1_000);
        assert_eq!(state.next_update, 2_000 + retry_delay(1, true).as_secs());
        assert!(state.last_error.unwrap().contains("500"));
        assert_eq!(body.unwrap(), LINKS.as_bytes());
        assert_eq!(source.cached(&store).unwrap().nodes.len(), 2);
    }

    /// Панель, которая первым запросом отдаёт рабочий ответ, а дальше — `later`.
    fn then_answers(later: String) -> FakePanel {
        let calls = Arc::new(AtomicU32::new(0));
        FakePanel::start(move |_| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                http_response("200 OK", &[], LINKS)
            } else {
                later.clone()
            }
        })
    }

    fn after_a_good_answer(later: String, failures: u32) -> Refresh {
        let panel = then_answers(later);
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("after-good");
        let store = store(&temp);
        assert!(matches!(
            refresh(&source, &store, 0, 1_000).outcome,
            Outcome::Applied(_)
        ));
        refresh(&source, &store, failures, 2_000)
    }

    const STUB_LINK: &str =
        "vless://00000000-0000-0000-0000-000000000000@0.0.0.0:1?security=none#Expired%20stub\n";
    const HOURS: u64 = 3_600;

    #[test]
    fn server_errors_back_off_with_a_ceiling() {
        let later = http_response("503 Service Unavailable", &[], "");
        let first = after_a_good_answer(later.clone(), 0);
        assert!(matches!(first.outcome, Outcome::Failed(_)));
        assert_eq!(first.next_in, Duration::from_secs(30));
        let fifth = after_a_good_answer(later.clone(), 4);
        assert_eq!(fifth.next_in, Duration::from_secs(480));
        let endless = after_a_good_answer(later, 50);
        assert_eq!(endless.next_in, Duration::from_secs(30 * 60));
    }

    #[test]
    fn a_stub_is_rechecked_hourly_however_often_it_repeats() {
        let later = http_response("200 OK", &[("x-hwid-max-devices-reached", "true")], "");
        for failures in [0, 1, 7, 50] {
            let done = after_a_good_answer(later.clone(), failures);
            assert!(matches!(done.outcome, Outcome::Rejected(_)), "{failures}");
            assert_eq!(done.next_in, Duration::from_secs(HOURS), "{failures}");
        }
    }

    #[test]
    fn a_stub_node_list_is_rechecked_within_an_hour_whatever_the_provider_asks() {
        let later = http_response("200 OK", &[("profile-update-interval", "24")], STUB_LINK);
        let done = after_a_good_answer(later, 5);
        let Outcome::Rejected(reason) = done.outcome else {
            panic!("заглушка должна быть отклонена");
        };
        assert!(reason.contains("Expired stub"), "{reason}");
        assert_eq!(done.next_in, Duration::from_secs(HOURS));
    }

    #[test]
    fn client_errors_are_rechecked_hourly() {
        for status in ["403 Forbidden", "404 Not Found", "410 Gone"] {
            let done = after_a_good_answer(http_response(status, &[], ""), 3);
            assert!(matches!(done.outcome, Outcome::Rejected(_)), "{status}");
            assert_eq!(done.next_in, Duration::from_secs(HOURS), "{status}");
        }
    }

    #[test]
    fn too_many_requests_waits_at_least_as_long_as_the_panel_asks() {
        let plain = http_response("429 Too Many Requests", &[], "");
        let done = after_a_good_answer(plain, 0);
        assert!(matches!(done.outcome, Outcome::Rejected(_)));
        assert_eq!(done.next_in, Duration::from_secs(HOURS));

        let short = http_response("429 Too Many Requests", &[("Retry-After", "60")], "");
        assert_eq!(
            after_a_good_answer(short, 0).next_in,
            Duration::from_secs(HOURS)
        );
        let long = http_response("429 Too Many Requests", &[("Retry-After", "172800")], "");
        assert_eq!(
            after_a_good_answer(long, 0).next_in,
            Duration::from_secs(24 * HOURS)
        );
    }

    #[test]
    fn a_server_error_waits_for_retry_after_when_it_is_longer() {
        let later = http_response("503 Service Unavailable", &[("Retry-After", "3600")], "");
        let done = after_a_good_answer(later, 0);
        assert!(matches!(done.outcome, Outcome::Failed(_)));
        assert_eq!(done.next_in, Duration::from_secs(HOURS));
    }

    #[test]
    fn a_shorter_configured_interval_applies_to_stubs_a_longer_one_does_not() {
        for (configured, expected) in [("6h", HOURS), ("30m", 1_800), ("10m", 600)] {
            let panel = then_answers(http_response(
                "200 OK",
                &[("x-hwid-max-devices-reached", "true")],
                "",
            ));
            let extra = format!("update_interval = \"{configured}\"\n");
            let config = config_for(&panel.url("/sub/token1234"), &extra);
            let source = Source::new(&config, &config.subscriptions[0], MACHINE_ID).unwrap();
            let temp = TempDir::new("configured");
            let store = store(&temp);
            refresh(&source, &store, 0, 1_000);
            let done = refresh(&source, &store, 0, 2_000);
            assert!(matches!(done.outcome, Outcome::Rejected(_)), "{configured}");
            assert_eq!(done.next_in, Duration::from_secs(expected), "{configured}");
            assert_eq!(
                store.load("тест").0.next_update,
                2_000 + expected,
                "{configured}"
            );
        }
    }

    #[test]
    fn the_last_known_provider_interval_comes_from_the_cache() {
        let panel = FakePanel::start(|_| {
            http_response("200 OK", &[("profile-update-interval", "3")], LINKS)
        });
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("known-interval");
        let store = store(&temp);
        let (state, body) = store.load("тест");
        assert_eq!(last_known_interval(&state, body.as_deref()), None);

        refresh(&source, &store, 0, 1_000);
        let (state, body) = store.load("тест");
        assert_eq!(
            last_known_interval(&state, body.as_deref()),
            Some(Duration::from_secs(3 * HOURS))
        );
        assert_eq!(last_known_interval(&state, None), None);
    }

    #[test]
    fn an_unreachable_panel_backs_off_and_keeps_the_cache() {
        let panel = FakePanel::start(|_| http_response("200 OK", &[], LINKS));
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("unreachable");
        let store = store(&temp);
        assert!(matches!(
            refresh(&source, &store, 0, 1_000).outcome,
            Outcome::Applied(_)
        ));
        drop(panel);

        let failed = refresh(&source, &store, 2, 3_000);
        assert!(matches!(failed.outcome, Outcome::Failed(_)));
        assert_eq!(failed.next_in, retry_delay(3, true));
        assert!(store.load("тест").0.last_error.is_some());
        assert_eq!(source.cached(&store).unwrap().nodes.len(), 2);
    }

    #[test]
    fn without_a_cache_retries_are_capped_lower() {
        let panel = FakePanel::start(|_| http_response("200 OK", &[], LINKS));
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        drop(panel);
        let temp = TempDir::new("cold");
        let store = store(&temp);
        let failed = refresh(&source, &store, 50, 1_000);
        assert!(matches!(failed.outcome, Outcome::Failed(_)));
        assert_eq!(failed.next_in, Duration::from_secs(10 * 60));
    }

    #[test]
    fn the_fallback_from_an_earlier_answer_is_used_when_the_main_address_dies() {
        let reserve = FakePanel::start(|_| http_response("200 OK", &[], LINKS));
        let reserve_url = reserve.url("/reserve/tokenABCD");
        let main = FakePanel::start(move |_| {
            http_response("200 OK", &[("Fallback-Url", reserve_url.as_str())], LINKS)
        });
        let (_, source) = source_for(&main.url("/sub/token1234"));
        let temp = TempDir::new("fallback");
        let store = store(&temp);
        assert!(matches!(
            refresh(&source, &store, 0, 1_000).outcome,
            Outcome::Applied(_)
        ));
        assert_eq!(
            store.load("тест").0.fallback_url,
            Some(reserve.url("/reserve/tokenABCD"))
        );
        assert_eq!(reserve.requests(), Vec::<String>::new());
        drop(main);

        let second = refresh(&source, &store, 0, 2_000);
        assert!(matches!(second.outcome, Outcome::Applied(_)));
        assert_eq!(reserve.requests().len(), 1);
    }

    #[test]
    fn a_relative_redirect_over_a_real_socket_reaches_the_answer() {
        let panel = FakePanel::start(|head| {
            if head.starts_with("GET /sub/token1234 ") {
                http_response("302 Found", &[("Location", "../moved/tokenZZZZ")], "")
            } else {
                http_response("200 OK", &[], LINKS)
            }
        });
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("redirect");
        let store = store(&temp);
        assert!(matches!(
            refresh(&source, &store, 0, 1_000).outcome,
            Outcome::Applied(_)
        ));
        let requests = panel.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("GET /moved/tokenZZZZ HTTP/1.1\r\n"));
    }

    #[test]
    fn a_cache_for_another_link_is_not_used() {
        let panel = FakePanel::start(|_| http_response("200 OK", &[], LINKS));
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("foreign");
        let store = store(&temp);
        refresh(&source, &store, 0, 1_000);
        assert!(source.cached(&store).is_some());

        let (_, other) = source_for(&panel.url("/sub/another5678"));
        assert!(other.cached(&store).is_none());
    }

    #[test]
    fn a_stub_answer_does_not_replace_the_cache() {
        let calls = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&calls);
        let panel = FakePanel::start(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                http_response("200 OK", &[], LINKS)
            } else {
                http_response("200 OK", &[("x-hwid-max-devices-reached", "true")], "")
            }
        });
        let (_, source) = source_for(&panel.url("/sub/token1234"));
        let temp = TempDir::new("stub");
        let store = store(&temp);
        refresh(&source, &store, 0, 1_000);
        let second = refresh(&source, &store, 0, 2_000);
        assert!(matches!(second.outcome, Outcome::Rejected(_)));
        assert_eq!(store.load("тест").1.unwrap(), LINKS.as_bytes());
    }
}
