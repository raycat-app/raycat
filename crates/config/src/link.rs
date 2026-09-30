use std::net::Ipv6Addr;

const MAX_LEN: usize = 2048;
const BAD_HOST: &str = "в ссылке некорректный адрес сервера";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheme {
    Http,
    Https,
}

pub(crate) fn check(url: &str) -> Result<Scheme, &'static str> {
    if url.is_empty() {
        return Err("ссылка пустая");
    }
    if url.len() > MAX_LEN {
        return Err("ссылка длиннее 2048 символов");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("в ссылке есть пробелы или управляющие символы");
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("ожидается ссылка вида https://адрес/путь")?;
    let scheme = if scheme.eq_ignore_ascii_case("https") {
        Scheme::Https
    } else if scheme.eq_ignore_ascii_case("http") {
        Scheme::Http
    } else {
        return Err("поддерживаются только ссылки http:// и https://");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("логин и пароль внутри ссылки не поддерживаются");
    }
    check_authority(authority)?;
    Ok(scheme)
}

fn check_authority(authority: &str) -> Result<(), &'static str> {
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, tail) = inner.split_once(']').ok_or(BAD_HOST)?;
        if host.parse::<Ipv6Addr>().is_err() {
            return Err(BAD_HOST);
        }
        let port = match tail {
            "" => None,
            tail => Some(tail.strip_prefix(':').ok_or(BAD_HOST)?),
        };
        (host, port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if !host
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(BAD_HOST);
        }
        (host, port)
    };
    if host.is_empty() {
        return Err("в ссылке нет адреса сервера");
    }
    if let Some(port) = port
        && !port.parse::<u16>().is_ok_and(|n| n != 0)
    {
        return Err("в ссылке некорректный порт");
    }
    Ok(())
}

/// `https://хост/…abcd`: хост и четыре последних символа пути, без запроса.
pub(crate) fn mask(url: &str) -> String {
    let url = url.trim();
    let split = url.split_once("://").filter(|(scheme, _)| {
        !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic())
    });
    let Some((scheme, rest)) = split else {
        return format!("…{}", tail(url));
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest
        .get(..authority_end)
        .and_then(|authority| authority.rsplit('@').next())
        .unwrap_or_default();
    let path = rest
        .get(authority_end..)
        .and_then(|target| target.split(['?', '#']).next())
        .unwrap_or_default();
    format!("{}://{host}/…{}", scheme.to_ascii_lowercase(), tail(path))
}

fn tail(text: &str) -> String {
    let skip = text.chars().count().saturating_sub(4);
    text.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_links() {
        assert_eq!(check("https://sub.example.com/a/b?c=d"), Ok(Scheme::Https));
        assert_eq!(check("HTTPS://example.com"), Ok(Scheme::Https));
        assert_eq!(check("http://203.0.113.7:8080/x"), Ok(Scheme::Http));
        assert_eq!(check("https://[2001:db8::1]:443/x"), Ok(Scheme::Https));
        assert_eq!(check("https://пример.рф/x"), Ok(Scheme::Https));
    }

    #[test]
    fn rejects_links() {
        for bad in [
            "",
            "example.com/sub",
            "ftp://example.com/sub",
            "https://",
            "https:///path",
            "https://user:pass@example.com/",
            "https://exa mple.com/",
            "https://example.com/\r\nHost: x",
            "https://example.com:0/",
            "https://example.com:99999/",
            "https://example.com:/",
            "https://exa*mple.com/",
            "https://[not-an-address]/",
            "https://[::1/",
        ] {
            assert!(check(bad).is_err(), "{bad:?}");
        }
        assert!(check(&format!("https://example.com/{}", "a".repeat(3000))).is_err());
    }

    #[test]
    fn masks_links() {
        assert_eq!(
            mask("https://sub.example.com/api/sub/AbCdEfGh1234?x=secretquery"),
            "https://sub.example.com/…1234"
        );
        assert_eq!(
            mask("HTTP://Example.com:8080/tok"),
            "http://Example.com:8080/…/tok"
        );
        assert_eq!(mask("https://example.com"), "https://example.com/…");
        assert_eq!(
            mask("https://example.com/?token=abcdef"),
            "https://example.com/…/"
        );
        assert_eq!(
            mask("https://user:secret@example.com/x/abcd"),
            "https://example.com/…abcd"
        );
        assert_eq!(
            mask("  https://example.com/sub/wxyz#frag "),
            "https://example.com/…wxyz"
        );
    }

    #[test]
    fn masks_things_that_are_not_links() {
        assert_eq!(mask("just-a-token-1234"), "…1234");
        assert_eq!(mask(""), "…");
        assert_eq!(mask("ab"), "…ab");
    }
}
