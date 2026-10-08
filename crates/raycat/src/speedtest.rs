//! Тест скорости через VPN: аргументы, расчёт скорости, подсказка и замер одного потока.
//! Замеры ведёт демон (`daemon/measure.rs`): он закрепляет узел и качает через служебный
//! вход xray. Здесь то, что от демона не зависит.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use raycat_http::{Client, Request, Scheme, Url};
use raycat_proto::{SpeedtestRequest, SpeedtestRun};
use raycat_xray::{Credentials, SpeedtestInbound};
use ring::rand::{SecureRandom, SystemRandom};

use crate::util::sanitize;

pub(crate) const DEFAULT_SIZE: u64 = 25_000_000;
const MIN_SIZE: u64 = 1_000_000;
const MAX_SIZE: u64 = 200_000_000;
const MAX_STREAMS: u8 = 8;
/// Без `--streams` два замера: один поток и четыре.
const DEFAULT_STREAMS: [u8; 2] = [1, 4];
const DEFAULT_URL: &str = "https://speed.cloudflare.com/__down";
/// Во сколько раз четыре потока должны быть быстрее одного, чтобы дать подсказку.
const SPLIT_RATIO: f64 = 1.5;
/// Срок одного потока: замер не длиннее него, потому что потоки идут параллельно.
pub(crate) const STREAM_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const PROGRESS_STEP: Duration = Duration::from_millis(250);

/// Размер вида `25MB`, `25MiB`, `512KiB`, `1000B`: число и обязательная единица.
pub(crate) fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(digits_end);
    let number: u64 = digits.parse().map_err(|_| {
        format!(
            "размер «{}» не понят: укажите число и единицу, например 25MB",
            sanitize(text)
        )
    })?;
    let factor: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => 1,
        "kb" => 1_000,
        "kib" => 1_024,
        "mb" => 1_000_000,
        "mib" => 1_048_576,
        _ => {
            return Err(format!(
                "единица «{}» не поддерживается: B, KB, KiB, MB или MiB",
                sanitize(unit)
            ));
        }
    };
    let bytes = number
        .checked_mul(factor)
        .ok_or_else(|| format!("размер «{}» слишком большой", sanitize(text)))?;
    check_size(bytes)?;
    Ok(bytes)
}

fn check_size(bytes: u64) -> Result<u64, String> {
    if bytes < MIN_SIZE {
        return Err("размер теста не меньше 1 МБ".to_owned());
    }
    if bytes > MAX_SIZE {
        return Err("размер теста не больше 200 МБ".to_owned());
    }
    Ok(bytes)
}

/// Адрес для `--url`: только https. Возвращает адрес без пробелов по краям.
pub(crate) fn check_url(text: &str) -> Result<String, String> {
    let text = text.trim();
    let url = Url::parse(text).map_err(|error| format!("адрес не разобран: {error:#}"))?;
    if !matches!(url.scheme, Scheme::Https) {
        return Err("адрес теста должен начинаться с https://".to_owned());
    }
    Ok(text.to_owned())
}

/// Проверка запроса `POST /v1/speedtest`; текст ошибки на русском.
pub(crate) fn check(request: &SpeedtestRequest) -> Result<(), String> {
    check_size(request.size)?;
    if let Some(streams) = request.streams
        && !(1..=MAX_STREAMS).contains(&streams)
    {
        return Err(format!("потоков может быть от 1 до {MAX_STREAMS}"));
    }
    if let Some(url) = &request.url {
        check_url(url)?;
    }
    Ok(())
}

/// Число потоков в каждом замере: `--streams N` — один замер с N потоками, без него — 1 и 4.
pub(crate) fn runs(streams: Option<u8>) -> Vec<u8> {
    streams.map_or_else(|| DEFAULT_STREAMS.to_vec(), |count| vec![count])
}

/// Объём одного потока: общий объём делится поровну.
pub(crate) fn share(size: u64, streams: u8) -> u64 {
    size / u64::from(streams.max(1))
}

/// Служебный вход с фиксированными учётными данными для тестов сборки конфига.
#[cfg(test)]
pub(crate) fn test_inbound() -> SpeedtestInbound {
    SpeedtestInbound {
        port: 10_086,
        credentials: Credentials {
            user: "speedtest".to_owned(),
            password: "test-password".to_owned(),
        },
    }
}

/// Служебный вход со свежими учётными данными. Пароль случайный на каждый запуск демона,
/// живёт только в памяти и в конфиге xray, который записан с правами 0600.
pub(crate) fn new_inbound(port: u16) -> Result<SpeedtestInbound> {
    let mut secret = [0u8; 32];
    SecureRandom::fill(&SystemRandom::new(), &mut secret)
        .map_err(|_| anyhow!("не удалось получить случайные числа для теста скорости"))?;
    let password = secret.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    Ok(SpeedtestInbound {
        port,
        credentials: Credentials {
            user: "speedtest".to_owned(),
            password,
        },
    })
}

/// Заголовок `Proxy-Authorization` для служебного входа.
pub(crate) fn proxy_authorization(credentials: &Credentials) -> String {
    let pair = format!("{}:{}", credentials.user, credentials.password);
    format!("Basic {}", STANDARD.encode(pair))
}

/// Адрес одного потока. У тестового сервера размер задаётся параметром `bytes`; свой адрес
/// качается как есть, до `share` байт или до конца ответа.
pub(crate) fn stream_url(custom: Option<&str>, share: u64) -> Result<Url> {
    match custom {
        Some(text) => Url::parse(text),
        None => Url::parse(&format!("{DEFAULT_URL}?bytes={share}")),
    }
}

/// Доля `done` от `total` в процентах, не больше 100.
pub(crate) fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    u8::try_from((done.saturating_mul(100) / total).min(100)).unwrap_or(100)
}

/// Мегабиты в секунду (10^6 бит/с).
#[allow(clippy::cast_precision_loss)]
pub(crate) fn mbps(bytes: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / seconds / 1_000_000.0
}

/// Подсказка, если четыре потока заметно быстрее одного: провайдер ограничивает соединение.
pub(crate) fn hint(runs: &[SpeedtestRun]) -> Option<String> {
    let single = runs.iter().find(|run| run.streams == 1)?;
    let multi = runs.iter().find(|run| run.streams == 4)?;
    (single.mbps > 0.0 && multi.mbps >= single.mbps * SPLIT_RATIO).then(|| {
        "Провайдер, похоже, ограничивает скорость одного соединения: попробуйте xray.xhttp_connections (для узлов XHTTP)"
            .to_owned()
    })
}

/// Что принёс один поток: объём и моменты первого и последнего байта тела.
#[derive(Debug)]
pub(crate) struct Sample {
    bytes: u64,
    started: Instant,
    first: Instant,
    last: Instant,
}

/// Один поток: один запрос, не больше `share` байт. `done` растёт по мере получения,
/// по нему считается прогресс замера.
pub(crate) fn fetch(client: &Client, url: &Url, share: u64, done: &AtomicU64) -> Result<Sample> {
    let headers = request_headers(url);
    let request = Request {
        method: "GET",
        target: &url.target,
        headers: &headers,
        body: &[],
    };
    let started = Instant::now();
    let mut first = None;
    let mut last = started;
    let mut bytes = 0;
    let response = client.stream(url, &request, &mut |piece| {
        let now = Instant::now();
        first.get_or_insert(now);
        last = now;
        let room = share.saturating_sub(bytes);
        let take = u64::try_from(piece.len()).unwrap_or(u64::MAX).min(room);
        bytes += take;
        done.fetch_add(take, Ordering::Relaxed);
        bytes < share
    })?;
    if !(200..300).contains(&response.status) {
        bail!("сервер ответил {} {}", response.status, response.reason);
    }
    let Some(first) = first else {
        bail!("сервер не прислал тела ответа");
    };
    Ok(Sample {
        bytes,
        started,
        first,
        last,
    })
}

fn request_headers(url: &Url) -> Vec<(String, String)> {
    vec![
        ("Host".to_owned(), url.host_header()),
        (
            "User-Agent".to_owned(),
            format!("raycat/{}", env!("CARGO_PKG_VERSION")),
        ),
        ("Accept".to_owned(), "*/*".to_owned()),
        ("Accept-Encoding".to_owned(), "identity".to_owned()),
        ("Connection".to_owned(), "close".to_owned()),
    ]
}

/// Сводка замера: объём всех потоков, время от первого байта до последнего, скорость и
/// среднее время до первого байта.
pub(crate) fn summarize(streams: u8, samples: &[Sample]) -> SpeedtestRun {
    let bytes: u64 = samples.iter().map(|sample| sample.bytes).sum();
    let start = samples.iter().map(|sample| sample.first).min();
    let end = samples.iter().map(|sample| sample.last).max();
    let seconds = match (start, end) {
        (Some(start), Some(end)) => end.saturating_duration_since(start).as_secs_f64(),
        _ => 0.0,
    };
    let ttfb_total: u64 = samples
        .iter()
        .map(|sample| millis(sample.first.saturating_duration_since(sample.started)))
        .sum();
    let count = u64::try_from(samples.len()).unwrap_or(1).max(1);
    SpeedtestRun {
        streams,
        bytes,
        seconds,
        mbps: mbps(bytes, seconds),
        ttfb_ms: ttfb_total / count,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn run(streams: u8, mbps: f64) -> SpeedtestRun {
        SpeedtestRun {
            streams,
            bytes: 1,
            seconds: 1.0,
            mbps,
            ttfb_ms: 1,
        }
    }

    #[test]
    fn sizes_need_a_unit_and_a_sane_range() {
        assert_eq!(parse_size("25MB"), Ok(25_000_000));
        assert_eq!(parse_size("25mib"), Ok(26_214_400));
        assert_eq!(parse_size(" 2 MB "), Ok(2_000_000));
        assert_eq!(parse_size("1000000B"), Ok(1_000_000));
        assert!(parse_size("25").unwrap_err().contains("единица"));
        assert!(parse_size("1.5MB").is_err());
        assert!(parse_size("MB").unwrap_err().contains("число"));
        assert!(parse_size("999KB").unwrap_err().contains("не меньше 1 МБ"));
        assert!(
            parse_size("201MB")
                .unwrap_err()
                .contains("не больше 200 МБ")
        );
        assert!(parse_size("99999999999999999999GB").is_err());
    }

    #[test]
    fn urls_must_be_https() {
        assert!(check_url("https://speed.example.com/file.bin").is_ok());
        assert!(check_url("http://speed.example.com/file.bin").is_err());
        assert!(check_url("ftp://speed.example.com/file.bin").is_err());
        assert!(check_url("не адрес").is_err());
    }

    #[test]
    fn a_request_is_checked_before_it_runs() {
        let ok = SpeedtestRequest {
            node: None,
            size: DEFAULT_SIZE,
            streams: Some(8),
            url: None,
        };
        assert_eq!(check(&ok), Ok(()));
        let many = SpeedtestRequest {
            streams: Some(9),
            ..ok.clone()
        };
        assert!(check(&many).is_err());
        let none = SpeedtestRequest {
            streams: Some(0),
            ..ok.clone()
        };
        assert!(check(&none).is_err());
        let plain = SpeedtestRequest {
            url: Some("http://example.com/".to_owned()),
            ..ok
        };
        assert!(check(&plain).is_err());
    }

    #[test]
    fn one_stream_or_the_default_pair() {
        assert_eq!(runs(None), vec![1, 4]);
        assert_eq!(runs(Some(3)), vec![3]);
        assert_eq!(share(25_000_000, 3), 8_333_333);
        assert_eq!(share(25_000_000, 4), 6_250_000);
    }

    #[test]
    fn speed_is_megabits_per_second() {
        assert!((mbps(125_000, 1.0) - 1.0).abs() < 1e-9);
        assert!((mbps(2_000_000, 1.0) - 16.0).abs() < 1e-9);
        assert!(mbps(10, 0.0).abs() < 1e-9);
    }

    #[test]
    fn progress_is_a_bounded_percentage() {
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(300, 200), 100);
        assert_eq!(percent(0, 0), 0);
    }

    #[test]
    fn a_run_is_summarized_from_its_streams() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let samples = [
            Sample {
                bytes: 1_000_000,
                started: start,
                first: at(10),
                last: at(1010),
            },
            Sample {
                bytes: 1_000_000,
                started: start,
                first: at(20),
                last: at(1010),
            },
        ];
        let summary = summarize(2, &samples);
        assert_eq!((summary.streams, summary.bytes), (2, 2_000_000));
        assert!((summary.seconds - 1.0).abs() < 1e-9);
        assert!((summary.mbps - 16.0).abs() < 1e-9);
        assert_eq!(summary.ttfb_ms, 15);
        assert!(summarize(1, &[]).mbps.abs() < 1e-9);
    }

    #[test]
    fn four_streams_much_faster_than_one_get_a_hint() {
        assert!(hint(&[run(1, 10.0), run(4, 40.0)]).is_some());
        assert!(hint(&[run(1, 10.0), run(4, 12.0)]).is_none());
        assert!(hint(&[run(1, 10.0)]).is_none());
        assert!(hint(&[run(4, 40.0)]).is_none());
        assert!(hint(&[run(1, 0.0), run(4, 40.0)]).is_none());
    }

    /// Отдаёт один ответ с телом из `body` и закрывает соединение.
    fn serve_once(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut byte = [0u8; 1];
            while !raw.ends_with(b"\r\n\r\n") {
                if sock.read(&mut byte).unwrap() == 0 {
                    break;
                }
                raw.push(byte[0]);
            }
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
            sock.write_all(head.as_bytes()).unwrap();
            sock.write_all(body.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn a_stream_stops_at_its_share_and_counts_the_bytes() {
        let port = serve_once("0123456789");
        let url = Url::parse(&format!("http://127.0.0.1:{port}/file")).unwrap();
        let done = AtomicU64::new(0);
        let sample = fetch(&Client::default(), &url, 4, &done).unwrap();
        assert_eq!(sample.bytes, 4);
        assert_eq!(done.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn every_start_gets_its_own_password() {
        let first = new_inbound(1).unwrap();
        let second = new_inbound(1).unwrap();
        assert_eq!(first.credentials.password.len(), 64);
        assert!(first.credentials.password.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(first.credentials.password, second.credentials.password);
        assert_eq!(first.port, 1);
    }

    #[test]
    fn the_proxy_header_is_basic_auth() {
        let credentials = Credentials {
            user: "user".to_owned(),
            password: "pass".to_owned(),
        };
        assert_eq!(proxy_authorization(&credentials), "Basic dXNlcjpwYXNz");
    }
}
