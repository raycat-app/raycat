/// Маска имени узла: `*` — любая последовательность символов, `?` — один символ,
/// регистр не учитывается. `\*`, `\?` и `\\` — сами эти символы; `escape_literal`
/// превращает произвольное имя в маску, которая совпадает только с ним.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    tokens: Vec<Token>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Lit(char),
    Star,
    One,
}

impl Pattern {
    pub fn new(raw: &str) -> Self {
        let folded: Vec<char> = raw.to_lowercase().chars().collect();
        let mut tokens = Vec::with_capacity(folded.len());
        let mut index = 0;
        while let Some(&c) = folded.get(index) {
            index += 1;
            let token = match c {
                '\\' => match folded.get(index) {
                    Some(&next @ ('*' | '?' | '\\')) => {
                        index += 1;
                        Token::Lit(next)
                    }
                    _ => Token::Lit('\\'),
                },
                '*' => Token::Star,
                '?' => Token::One,
                other => Token::Lit(other),
            };
            tokens.push(token);
        }
        Self {
            raw: raw.to_owned(),
            tokens,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Совпадает с исходным именем, безопасным (флаги → коды, эмодзи убраны) или
    /// безопасным без кода страны. Так «Обход*» ловит «🇩🇪 Обход 1».
    pub fn matches(&self, name: &str) -> bool {
        crate::names::variants(name)
            .iter()
            .any(|variant| self.matches_exactly(variant))
    }

    fn matches_exactly(&self, name: &str) -> bool {
        let text: Vec<char> = name.to_lowercase().chars().collect();
        glob_match(&self.tokens, &text)
    }
}

/// Маска, которая совпадает с именем буквально: `*`, `?` и `\` экранируются.
pub fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '?' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Итеративное сопоставление с откатом к последней `*`: работает за время,
/// близкое к линейному, и не уходит в экспоненту на длинных именах.
fn glob_match(pattern: &[Token], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(Token::Star) => {
                star = Some((p, t));
                p += 1;
            }
            Some(Token::One) => {
                p += 1;
                t += 1;
            }
            Some(Token::Lit(c)) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|token| *token == Token::Star)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let p = Pattern::new("*🇳🇱*");
        assert!(p.matches("🇳🇱 Netherlands 1"));
        assert!(!p.matches("🇩🇪 Germany"));
        assert!(Pattern::new("nl-?").matches("NL-1"));
        assert!(!Pattern::new("nl-?").matches("NL-10"));
        assert!(Pattern::new("*германия*").matches("🇩🇪 Германия (Франкфурт)"));
        assert!(Pattern::new("a*b*c").matches("aXXbYYc"));
        assert!(!Pattern::new("a*b*c").matches("aXXbYY"));
        assert!(Pattern::new("*").matches(""));
        assert!(Pattern::new("exact").matches("EXACT"));
        assert!(!Pattern::new("exact").matches("exactly"));
    }

    #[test]
    fn question_mark_needs_a_character() {
        assert!(!Pattern::new("?").matches(""));
        assert!(Pattern::new("??").matches("ЯЯ"));
        assert!(!Pattern::new("??").matches("Я"));
    }

    #[test]
    fn many_stars_do_not_blow_up() {
        let pattern = Pattern::new(&"*a".repeat(40));
        assert!(!pattern.matches(&"a".repeat(39)));
        assert!(pattern.matches(&"a".repeat(40)));
    }

    #[test]
    fn a_mask_sees_the_flag_emoji_and_code_free_variants() {
        assert!(Pattern::new("Обход*").matches("🇩🇪 Обход 1"));
        assert!(Pattern::new("DE Обход*").matches("🇩🇪 Обход 1"));
        assert!(Pattern::new("обход*").matches("🇩🇪 ОБХОД 1"));
        assert!(Pattern::new("Быстрый").matches("⚡️ Быстрый"));
        assert!(Pattern::new("NL ?").matches("🇳🇱 ⚡️ Q"));
        assert!(!Pattern::new("Обход").matches("🇩🇪 Обход 1"));
        assert!(!Pattern::new("Германия*").matches("🇩🇪 Обход 1"));
    }

    #[test]
    fn a_question_mark_is_one_character_in_any_variant() {
        assert!(Pattern::new("NL-?").matches("🇳🇱 NL-1"));
        assert!(!Pattern::new("NL-?").matches("🇳🇱 NL-10"));
    }

    #[test]
    fn an_escaped_star_or_question_mark_is_a_plain_character() {
        assert!(Pattern::new(r"NL\*1").matches("NL*1"));
        assert!(!Pattern::new(r"NL\*1").matches("NL-1"));
        assert!(Pattern::new(r"a\?").matches("A?"));
        assert!(!Pattern::new(r"a\?").matches("ab"));
        assert!(Pattern::new(r"a\\b").matches(r"a\b"));
    }

    #[test]
    fn a_literal_mask_matches_only_its_own_name() {
        let name = "X*Y? 1";
        assert!(Pattern::new(&escape_literal(name)).matches(name));
        assert!(!Pattern::new(&escape_literal(name)).matches("XAY 1"));
    }

    #[test]
    fn keeps_the_original_text() {
        assert_eq!(Pattern::new("*NL*").as_str(), "*NL*");
    }
}
