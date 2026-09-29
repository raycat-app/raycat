//! Абсолютные ссылки `http(s)://`: разбор, разрешение `Location` при редиректах,
//! IDN в punycode, маскировка для логов.

use anyhow::{Result, anyhow, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
}

/// Абсолютная ссылка `http(s)://`, приведённая к виду, который дают браузеры,
/// dart:io и Node: хост в нижнем регистре, путь по умолчанию `/`, без фрагмента,
/// не-ASCII байты пути закодированы в процентах.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    /// Путь вместе с запросом, всегда начинается с `/`.
    pub target: String,
}

impl Url {
    /// Разбирает абсолютную ссылку. Ошибки никогда не повторяют ввод: ссылка
    /// подписки несёт токен доступа, а сообщения об ошибках попадают в логи.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        let (scheme, rest) = if let Some(rest) = strip_prefix_ci(input, "https://") {
            (Scheme::Https, rest)
        } else if let Some(rest) = strip_prefix_ci(input, "http://") {
            (Scheme::Http, rest)
        } else {
            bail!("неподдерживаемая ссылка: ожидается http:// или https://");
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, target) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.contains('@') {
            bail!("логин и пароль внутри ссылки не поддерживаются");
        }
        let (host, port) = split_host_port(authority)?;
        let host = normalize_host(host)?;
        let target = if target.starts_with('?') {
            format!("/{target}")
        } else {
            target.to_owned()
        };
        Ok(Self {
            scheme,
            host,
            port: port.unwrap_or(scheme.default_port()),
            target: encode_target(&target),
        })
    }

    /// Значение заголовка `Host`: порт указан, только если он не стандартный
    /// для схемы.
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Разрешает заголовок `Location` относительно этой ссылки (RFC 3986, п. 5.2).
    pub fn join(&self, location: &str) -> Result<Self> {
        let location = location.trim();
        let location = location.split('#').next().unwrap_or_default();
        if has_scheme(location) {
            return Self::parse(location);
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Self::parse(&format!("{}://{rest}", self.scheme.as_str()));
        }
        let base_path = self.target.split('?').next().unwrap_or("/");
        let target = if location.is_empty() {
            self.target.clone()
        } else if location.starts_with('?') {
            format!("{base_path}{location}")
        } else {
            let (path, query) = match location.split_once('?') {
                Some((path, query)) => (path, Some(query)),
                None => (location, None),
            };
            let merged = if path.starts_with('/') {
                path.to_owned()
            } else {
                let dir = &base_path[..=base_path.rfind('/').unwrap_or(0)];
                format!("{dir}{path}")
            };
            let mut target = remove_dot_segments(&merged);
            if let Some(query) = query {
                target.push('?');
                target.push_str(query);
            }
            target
        };
        Ok(Self {
            target: encode_target(&target),
            ..self.clone()
        })
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}://{}{}",
            self.scheme.as_str(),
            self.host_header(),
            self.target
        )
    }
}

/// Ссылка для логов: хост остаётся, из пути — только последние 4 символа, запрос
/// отбрасывается. Ссылка подписки — секрет.
pub fn redact(url: &Url) -> String {
    let path = url.target.split('?').next().unwrap_or_default();
    let tail: String = path
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}://{}/…{tail}", url.scheme.as_str(), url.host_header())
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Префикс `схема ":"` по RFC 3986: буква, затем буквы, цифры, `+`, `-`, `.`.
fn has_scheme(reference: &str) -> bool {
    let Some(colon) = reference.find(':') else {
        return false;
    };
    let scheme = &reference[..colon];
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
}

/// RFC 3986, п. 5.2.4, для абсолютного пути.
fn remove_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let last = segments.len().saturating_sub(1);
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        match *segment {
            "." | ".." => {
                if *segment == ".." {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

fn split_host_port(authority: &str) -> Result<(&str, Option<u16>)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| anyhow!("неверный IPv6-адрес в ссылке"))?;
        let port = match &rest[end + 1..] {
            "" => None,
            tail => Some(
                tail.strip_prefix(':')
                    .ok_or_else(|| anyhow!("неверный IPv6-адрес в ссылке"))?,
            ),
        };
        (&rest[..end], port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(p) if !p.is_empty() => Some(p.parse().map_err(|_| anyhow!("неверный порт в ссылке"))?),
        _ => None,
    };
    Ok((host, port))
}

/// Приводит хост к нижнему регистру, а интернационализированные имена
/// (`пример.рф`) — к ASCII (`xn--e1afmkfd.xn--p1ai`), как это делают браузеры, Node
/// и Qt до DNS, SNI и заголовка `Host`. Хост попадает в запрос как есть, поэтому
/// после этого допустимы только DNS-имена и IP-адреса: это же исключает подмену
/// заголовков.
fn normalize_host(host: &str) -> Result<String> {
    if host.is_empty() {
        bail!("в ссылке нет хоста");
    }
    let host = if host.is_ascii() {
        host.to_ascii_lowercase()
    } else {
        to_ascii_domain(host)
            .ok_or_else(|| anyhow!("хост ссылки не является допустимым доменным именем"))?
    };
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._:".contains(&b))
    {
        bail!("хост ссылки содержит недопустимые символы");
    }
    Ok(host)
}

/// IDNA `ToASCII` для типичного случая: нижний регистр, деление по (идеографическим)
/// точкам, punycode для каждой не-ASCII метки. Полная таблица UTS 46
/// (совместимые символы, `ß`, NFC) сознательно не подключена: кириллица, латиница с
/// диакритикой и подобные имена получаются так же, как в браузерах.
fn to_ascii_domain(host: &str) -> Option<String> {
    let host = host.to_lowercase();
    let labels: Vec<String> = host
        .split(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'])
        .map(|label| {
            if label.is_ascii() {
                Some(label.to_owned())
            } else {
                Some(format!("xn--{}", punycode(label)?))
            }
        })
        .collect::<Option<_>>()?;
    labels
        .iter()
        .all(|l| !l.is_empty() && l.len() <= 63)
        .then(|| labels.join("."))
}

/// Кодировщик punycode (RFC 3492, п. 6.3).
fn punycode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const T_MIN: u32 = 1;
    const T_MAX: u32 = 26;
    fn digit(d: u32) -> Option<char> {
        let code = if d < 26 {
            u32::from(b'a') + d
        } else {
            u32::from(b'0') + d - 26
        };
        char::from_u32(code)
    }
    fn adapt(delta: u32, points: u32, first: bool) -> u32 {
        let mut delta = if first { delta / 700 } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - T_MIN) * T_MAX) / 2 {
            delta /= BASE - T_MIN;
            k += BASE;
        }
        k + (BASE - T_MIN + 1) * delta / (delta + 38)
    }
    let code_points: Vec<u32> = input.chars().map(u32::from).collect();
    let total = u32::try_from(code_points.len()).ok()?;
    let mut out: String = input.chars().filter(char::is_ascii).collect();
    let basic = u32::try_from(out.len()).ok()?;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut handled) = (128u32, 0u32, 72u32, basic);
    while handled < total {
        let m = code_points.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;
        for &c in &code_points {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        T_MIN
                    } else if k >= bias + T_MAX {
                        T_MAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t))?);
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q)?);
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n += 1;
    }
    Some(out)
}

fn encode_target(target: &str) -> String {
    let mut out = String::with_capacity(target.len());
    for &b in target.as_bytes() {
        if (0x21..0x7f).contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let url = Url::parse("HTTPS://Sub.Example.com/abc?x=1#frag").unwrap();
        assert_eq!(url.scheme, Scheme::Https);
        assert_eq!(url.host, "sub.example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.target, "/abc?x=1");
        assert_eq!(url.host_header(), "sub.example.com");
        assert_eq!(url.to_string(), "https://sub.example.com/abc?x=1");

        let url = Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(url.target, "/");
        assert_eq!(url.host_header(), "127.0.0.1:8080");

        let url = Url::parse("http://[::1]:9090/version").unwrap();
        assert_eq!(url.host, "::1");
        assert_eq!(url.host_header(), "[::1]:9090");

        assert_eq!(Url::parse("https://h/?q").unwrap().target, "/?q");
        assert_eq!(
            Url::parse("https://h/путь").unwrap().target,
            "/%D0%BF%D1%83%D1%82%D1%8C"
        );
        assert!(Url::parse("ftp://h/").is_err());
        assert!(Url::parse("https://u:p@h/").is_err());
        assert!(Url::parse("https://пример..рф/").is_err());
        assert!(Url::parse("https://a b/").is_err());
        assert!(Url::parse("https://a\r\nX-Evil: 1/").is_err());
        assert!(Url::parse("https://[::1]x/").is_err());
    }

    #[test]
    fn internationalised_domains_become_punycode() {
        // Эталонные значения: примеры из RFC 3492 и то, что отправляют браузеры.
        assert_eq!(punycode("пример").as_deref(), Some("e1afmkfd"));
        assert_eq!(punycode("bücher").as_deref(), Some("bcher-kva"));
        assert_eq!(punycode("münchen").as_deref(), Some("mnchen-3ya"));
        assert_eq!(punycode("президент").as_deref(), Some("d1abbgf6aiiy"));
        let url = Url::parse("https://ПРИМЕР.рф:8443/sub/токен?x=1").unwrap();
        assert_eq!(url.host, "xn--e1afmkfd.xn--p1ai");
        assert_eq!(url.host_header(), "xn--e1afmkfd.xn--p1ai:8443");
        assert_eq!(url.target, "/sub/%D1%82%D0%BE%D0%BA%D0%B5%D0%BD?x=1");
        assert_eq!(
            Url::parse("https://sub.пример。рф/").unwrap().host,
            "sub.xn--e1afmkfd.xn--p1ai"
        );
    }

    #[test]
    fn url_errors_do_not_leak_the_input() {
        for bad in [
            "ftp://h/SECRET",
            "https://h:SECRET/",
            "https://SECRET\u{1}/",
        ] {
            let err = format!("{:#}", Url::parse(bad).unwrap_err());
            assert!(!err.contains("SECRET"), "{err}");
        }
    }

    #[test]
    fn joins_redirects() {
        let base = Url::parse("https://a.com/sub/abc?x").unwrap();
        let join = |loc: &str| base.join(loc).unwrap().to_string();
        assert_eq!(join("/new"), "https://a.com/new");
        assert_eq!(join("def"), "https://a.com/sub/def");
        assert_eq!(join("//b.com/z"), "https://b.com/z");
        assert_eq!(join("http://c.com:81/"), "http://c.com:81/");
        assert_eq!(join("HTTPS://D.com/q"), "https://d.com/q");
        assert_eq!(join("?y=1"), "https://a.com/sub/abc?y=1");
        assert_eq!(join(""), "https://a.com/sub/abc?x");
        assert_eq!(join("../up"), "https://a.com/up");
        assert_eq!(join("./same/../x?q=1#f"), "https://a.com/sub/x?q=1");
        assert_eq!(join("/../../etc"), "https://a.com/etc");
        // Относительная ссылка, в запросе которой лежит URL, не абсолютный URL.
        assert_eq!(
            join("/go?to=https://evil.com/"),
            "https://a.com/go?to=https://evil.com/"
        );
        assert!(base.join("javascript:alert(1)").is_err());
    }

    #[test]
    fn redacts_tokens() {
        let url = Url::parse("https://sub.example.com/api/sub/AbCdEfGh1234?x=1").unwrap();
        assert_eq!(redact(&url), "https://sub.example.com/…1234");
        let url = Url::parse("http://203.0.113.7:8080/s/abcd1234").unwrap();
        assert_eq!(redact(&url), "http://203.0.113.7:8080/…1234");
    }
}
