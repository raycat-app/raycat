/// Маска имени узла: `*` — любая последовательность символов, `?` — один символ,
/// регистр не учитывается.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    folded: Vec<char>,
}

impl Pattern {
    pub(crate) fn new(raw: &str) -> Self {
        Self {
            raw: raw.to_owned(),
            folded: raw.to_lowercase().chars().collect(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, name: &str) -> bool {
        let text: Vec<char> = name.to_lowercase().chars().collect();
        glob_match(&self.folded, &text)
    }
}

/// Итеративное сопоставление с откатом к последней `*`: работает за время,
/// близкое к линейному, и не уходит в экспоненту на длинных именах.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if *c == text[t] => {
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
    pattern[p..].iter().all(|c| *c == '*')
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
    fn keeps_the_original_text() {
        assert_eq!(Pattern::new("*NL*").as_str(), "*NL*");
    }
}
