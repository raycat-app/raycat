//! Маскировка ссылок подписки: ссылка — это пароль пользователя.

const TAIL: usize = 4;

/// Оставляет схему, хост и последние 4 символа пути: `https://host/…abcd`.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return format!("…{}", tail(url));
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, target) = rest.split_at(end);
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let path = target.split(['?', '#']).next().unwrap_or("");
    format!("{scheme}://{host}/…{}", tail(path.trim_matches('/')))
}

/// Маскирует в тексте секретные части ссылок подписки: панели повторяют ссылку в
/// `profile-web-page-url`, `fallback-url` и объявлениях.
pub fn redact_in(text: &str, urls: &[&str]) -> String {
    let mut found: Vec<&str> = urls.iter().copied().flat_map(secrets).collect();
    // Сначала длинные: короткий секрет может входить в длинный.
    found.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    found.dedup();
    let mut out = text.to_owned();
    for secret in found {
        out = out.replace(secret, &format!("…{}", tail(secret)));
    }
    out
}

fn tail(text: &str) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(TAIL)).collect()
}

/// Части пути и запроса, похожие на токен: не короче 8 символов, с цифрой или
/// заглавными и строчными буквами сразу (не слова вроде `subscription`), а также
/// последний сегмент пути.
fn secrets(url: &str) -> Vec<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let target = rest.find('/').map_or("", |index| &rest[index..]);
    let mut found: Vec<&str> = target
        .split(['/', '?', '&', '=', '#'])
        .filter(|part| looks_like_token(part))
        .collect();
    let path = target.split(['?', '#']).next().unwrap_or("");
    if let Some(last) = path.trim_end_matches('/').rsplit('/').next()
        && last.chars().count() >= 8
    {
        found.push(last);
    }
    found
}

fn looks_like_token(part: &str) -> bool {
    let has_digit = part.chars().any(|c| c.is_ascii_digit());
    let has_upper = part.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = part.chars().any(|c| c.is_ascii_lowercase());
    part.chars().count() >= 8 && (has_digit || (has_upper && has_lower))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_the_link_itself() {
        assert_eq!(
            redact("https://sub.example.com/api/sub/AbCdEfGh1234?x=1"),
            "https://sub.example.com/…1234"
        );
        assert_eq!(
            redact("https://user:pass@sub.example.com:8443/token/abcd/#frag"),
            "https://sub.example.com:8443/…abcd"
        );
        assert_eq!(redact("https://sub.example.com"), "https://sub.example.com/…");
        assert_eq!(redact("https://sub.example.com/"), "https://sub.example.com/…");
        assert_eq!(redact("https://sub.example.com/ab"), "https://sub.example.com/…ab");
        assert_eq!(redact("no scheme at all 1234"), "…1234");
        assert_eq!(redact(""), "…");
    }

    #[test]
    fn redacts_tokens_echoed_by_the_panel() {
        let urls = [
            "https://a.example.shop/cart/r2TReK2sLtYvCzk0",
            "https://moved.example.com/sub/4/00000000-0000-0000-0000-000000000000?key=Secret99",
        ];
        assert_eq!(
            redact_in("https://a.example.shop/cart/r2TReK2sLtYvCzk0", &urls),
            "https://a.example.shop/cart/…Czk0"
        );
        assert_eq!(
            redact_in(
                "https://mirror.example.net/sub/00000000-0000-0000-0000-000000000000?key=Secret99",
                &urls
            ),
            "https://mirror.example.net/sub/…0000?key=…et99"
        );
    }

    #[test]
    fn words_and_short_segments_stay_readable() {
        let urls = ["https://a.example.shop/cart/r2TReK2sLtYvCzk0"];
        let plain = "Продлить: https://example.com/subscription/cart/4";
        assert_eq!(redact_in(plain, &urls), plain);
        assert_eq!(redact_in("текст без ссылок", &urls), "текст без ссылок");
        assert_eq!(redact_in("текст", &[]), "текст");
    }

    #[test]
    fn last_path_segment_is_always_secret() {
        let urls = ["https://sub.example.com/abcdefghijkl"];
        assert_eq!(
            redact_in("см. https://sub.example.com/abcdefghijkl", &urls),
            "см. https://sub.example.com/…ijkl"
        );
    }
}
