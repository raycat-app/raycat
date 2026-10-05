//! Обратная подстановка: из готового значения заголовка и шаблона профиля —
//! значения подстановок.

use crate::template::{Part, Template, Var};

/// Кусок шаблона: известный текст или подстановка, значение которой ищется.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    Text(String),
    Free(Var),
}

pub(super) fn push_text(pieces: &mut Vec<Piece>, text: &str) {
    if let Some(Piece::Text(last)) = pieces.last_mut() {
        last.push_str(text);
    } else {
        pieces.push(Piece::Text(text.to_owned()));
    }
}

/// Куски шаблона; `known` возвращает значения подстановок, которые уже известны.
pub(super) fn pieces(template: &Template, known: impl Fn(Var) -> Option<String>) -> Vec<Piece> {
    let mut pieces = Vec::new();
    for part in &template.0 {
        match part {
            Part::Text(text) => push_text(&mut pieces, text),
            Part::Var(var) => match known(*var) {
                Some(value) => push_text(&mut pieces, &value),
                None => pieces.push(Piece::Free(*var)),
            },
        }
    }
    pieces
}

/// Значения подстановок, при которых `pieces` дают `text`. Две подстановки подряд
/// без текста между ними разделить нельзя, поэтому такой шаблон не разбирается.
pub(super) fn unrender(pieces: &[Piece], text: &str) -> Option<Vec<(Var, String)>> {
    let mut found = Vec::new();
    let mut rest = text;
    let mut iter = pieces.iter().peekable();
    while let Some(piece) = iter.next() {
        match piece {
            Piece::Text(expected) => rest = rest.strip_prefix(expected.as_str())?,
            Piece::Free(var) => {
                let end = match iter.peek() {
                    None => rest.len(),
                    Some(Piece::Text(next)) => rest.find(next.as_str())?,
                    Some(Piece::Free(_)) => return None,
                };
                if end == 0 {
                    return None;
                }
                found.push((*var, rest[..end].to_owned()));
                rest = &rest[end..];
            }
        }
    }
    rest.is_empty().then_some(found)
}

pub(super) fn value_of(found: &[(Var, String)], var: Var) -> Option<&str> {
    found
        .iter()
        .find(|(candidate, _)| *candidate == var)
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(source: &str) -> Template {
        Template::parse(source).unwrap()
    }

    fn nothing_known(_: Var) -> Option<String> {
        None
    }

    #[test]
    fn splits_values_around_the_literal_text() {
        let pieces = pieces(&template("{manufacturer} {model}"), nothing_known);
        let found = unrender(&pieces, "Google Pixel 8").unwrap();
        assert_eq!(value_of(&found, Var::Manufacturer), Some("Google"));
        assert_eq!(value_of(&found, Var::Model), Some("Pixel 8"));
    }

    #[test]
    fn known_values_become_text() {
        let known = |var: Var| (var == Var::Cpu).then(|| "x86_64".to_owned());
        let pieces = pieces(&template("{hostname}_{cpu}"), known);
        assert_eq!(
            pieces,
            [
                Piece::Free(Var::Hostname),
                Piece::Text("_x86_64".to_owned())
            ]
        );
        let found = unrender(&pieces, "runnervm1_x86_64").unwrap();
        assert_eq!(value_of(&found, Var::Hostname), Some("runnervm1"));
        assert!(unrender(&pieces, "runnervm1_arm64").is_none());
    }

    #[test]
    fn a_plain_template_is_a_plain_comparison() {
        let pieces = pieces(&template("gzip, deflate"), nothing_known);
        assert_eq!(unrender(&pieces, "gzip, deflate"), Some(Vec::new()));
        assert!(unrender(&pieces, "gzip").is_none());
        assert!(unrender(&pieces, "gzip, deflate!").is_none());
    }

    #[test]
    fn ambiguous_and_empty_matches_are_refused() {
        let adjacent = pieces(&template("{build}{tail}"), nothing_known);
        assert!(unrender(&adjacent, "12345").is_none());
        let single = pieces(&template("v{app_version}"), nothing_known);
        assert!(unrender(&single, "v").is_none());
        assert!(unrender(&single, "x1").is_none());
        assert!(unrender(&single, "v1.2").is_some());
    }
}
