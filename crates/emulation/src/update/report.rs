//! Понятные отчёты о том, чем захват отличается от профиля.

/// Построчная разница двух списков имён заголовков: `-` есть в профиле, но нет в
/// захвате, `+` есть в захвате, но нет в профиле, остальные строки совпали.
pub(super) fn diff_names(profile: &[String], capture: &[String]) -> Vec<String> {
    let (n, m) = (profile.len(), capture.len());
    let mut common = vec![vec![0_usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[i][j] = if profile[i] == capture[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut lines = Vec::new();
    while i < n && j < m {
        if profile[i] == capture[j] {
            lines.push(format!("    {}", profile[i]));
            i += 1;
            j += 1;
        } else if common[i + 1][j] >= common[i][j + 1] {
            lines.push(format!("  - {}", profile[i]));
            i += 1;
        } else {
            lines.push(format!("  + {}", capture[j]));
            j += 1;
        }
    }
    lines.extend(profile[i..].iter().map(|name| format!("  - {name}")));
    lines.extend(capture[j..].iter().map(|name| format!("  + {name}")));
    lines
}

/// Строки запроса, которые захват и профиль дают по-разному.
pub(super) fn diff_lines(capture: &str, rendered: &str) -> Vec<String> {
    let capture: Vec<&str> = capture.split("\r\n").collect();
    let rendered: Vec<&str> = rendered.split("\r\n").collect();
    let mut lines = Vec::new();
    for i in 0..capture.len().max(rendered.len()) {
        let (left, right) = (capture.get(i), rendered.get(i));
        if left != right {
            lines.push(format!("  захват:  {}", left.unwrap_or(&"(нет строки)")));
            lines.push(format!("  профиль: {}", right.unwrap_or(&"(нет строки)")));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn identical_lists_have_no_marks() {
        let list = names(&["Host", "User-Agent"]);
        assert_eq!(diff_names(&list, &list), ["    Host", "    User-Agent"]);
    }

    #[test]
    fn added_removed_and_renamed_headers_are_marked() {
        let profile = names(&["Host", "X-Old", "Accept"]);
        let capture = names(&["Host", "Accept", "X-New"]);
        assert_eq!(
            diff_names(&profile, &capture),
            ["    Host", "  - X-Old", "    Accept", "  + X-New"]
        );
        let renamed = names(&["Host", "x-old", "Accept"]);
        let diff = diff_names(&profile, &renamed);
        assert!(diff.contains(&"  - X-Old".to_owned()), "{diff:?}");
        assert!(diff.contains(&"  + x-old".to_owned()), "{diff:?}");
    }

    #[test]
    fn a_reordered_header_shows_as_moved() {
        let profile = names(&["A", "B", "C"]);
        let capture = names(&["B", "A", "C"]);
        let diff = diff_names(&profile, &capture);
        assert_eq!(
            diff.iter().filter(|line| line.starts_with("  -")).count(),
            1
        );
        assert_eq!(
            diff.iter().filter(|line| line.starts_with("  +")).count(),
            1
        );
    }

    #[test]
    fn lists_of_different_length_are_handled() {
        assert_eq!(diff_names(&[], &names(&["A"])), ["  + A"]);
        assert_eq!(diff_names(&names(&["A"]), &[]), ["  - A"]);
        assert!(diff_names(&[], &[]).is_empty());
    }

    #[test]
    fn only_different_lines_are_listed() {
        let lines = diff_lines(
            "GET / HTTP/1.1\r\nA: 1\r\nB: 2",
            "GET / HTTP/1.1\r\nA: 1\r\nB: 3",
        );
        assert_eq!(lines, ["  захват:  B: 2", "  профиль: B: 3"]);
        assert!(diff_lines("same", "same").is_empty());
        assert_eq!(diff_lines("a\r\nb", "a").len(), 2);
    }
}
