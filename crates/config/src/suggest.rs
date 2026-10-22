//! Подсказки при опечатках: ближайший допустимый вариант к введённому слову.

/// Ближайший к `value` вариант из `options` (регистр не важен) или `None`, если ни один не
/// похож достаточно.
pub(crate) fn nearest<'a>(value: &str, options: &[&'a str]) -> Option<&'a str> {
    let value = value.to_lowercase();
    options
        .iter()
        .map(|option| (distance(&value, &option.to_lowercase()), *option))
        .filter(|(found, option)| *found <= tolerance(option))
        .min_by_key(|(found, _)| *found)
        .map(|(_, option)| option)
}

/// Опечатка допустима на треть длины варианта, но не меньше одной правки: `hap` → `happ`.
fn tolerance(option: &str) -> usize {
    (option.chars().count() / 3).max(1)
}

/// Расстояние Левенштейна: сколько правок (вставка, удаление, замена) переводят `from` в `to`.
fn distance(from: &str, to: &str) -> usize {
    let target: Vec<char> = to.chars().collect();
    let mut row: Vec<usize> = (0..=target.len()).collect();
    for (line, from_char) in from.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = line + 1;
        for (column, to_char) in target.iter().enumerate() {
            let above = row[column + 1];
            row[column + 1] = if from_char == *to_char {
                diagonal
            } else {
                1 + diagonal.min(row[column]).min(above)
            };
            diagonal = above;
        }
    }
    row[target.len()]
}

/// Перечень вариантов для сообщения: `happ или incy`, `direct, proxy или block`.
pub(crate) fn join_or(options: &[&str]) -> String {
    match options {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} или {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_words_are_suggested() {
        assert_eq!(
            nearest("subscriptions", &["subscription", "selection"]),
            Some("subscription")
        );
        assert_eq!(nearest("hap", &["happ", "incy"]), Some("happ"));
        assert_eq!(nearest("INCY", &["happ", "incy"]), Some("incy"));
        assert_eq!(nearest("gatway", &["proxy", "gateway"]), Some("gateway"));
    }

    #[test]
    fn distant_words_get_no_suggestion() {
        assert_eq!(nearest("lvl", &["level"]), None);
        assert_eq!(nearest("", &["happ", "incy"]), None);
        assert_eq!(nearest("firefox", &["happ", "incy"]), None);
        assert_eq!(nearest("hap", &[]), None);
    }

    #[test]
    fn options_are_listed_in_russian() {
        assert_eq!(join_or(&["happ", "incy"]), "happ или incy");
        assert_eq!(
            join_or(&["direct", "proxy", "block"]),
            "direct, proxy или block"
        );
        assert_eq!(join_or(&[]), "");
    }
}
