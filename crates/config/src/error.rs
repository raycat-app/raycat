use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    field: String,
    message: String,
}

impl Problem {
    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// В контейнере с `cap_drop: [ALL]` у root нет CAP_DAC_OVERRIDE и он не читает чужие
/// файлы с правами 0600.
const PERMISSION_HINT: &str = "\nПодсказка: в контейнере с cap_drop: [ALL] root читает файл только по обычным правам доступа. \
Сделайте файл читаемым для всех (chmod 644, а каталог закройте от посторонних) \
или передайте его root (chown 0:0, права 600).";

#[derive(Debug)]
pub enum Error {
    Read { path: PathBuf, source: io::Error },
    Parse(String),
    Invalid(Vec<Problem>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    f,
                    "не удалось прочитать файл настроек {}: {source}",
                    path.display()
                )?;
                if source.kind() == io::ErrorKind::PermissionDenied {
                    f.write_str(PERMISSION_HINT)?;
                }
                Ok(())
            }
            Self::Parse(message) => write!(f, "не удалось разобрать файл настроек: {message}"),
            Self::Invalid(problems) => {
                f.write_str("ошибки в настройках:")?;
                for problem in problems {
                    write!(f, "\n  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse(_) | Self::Invalid(_) => None,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Problems(Vec<Problem>);

impl Problems {
    pub(crate) fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.0.push(Problem {
            field: field.into(),
            message: message.into(),
        });
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn into_error(self) -> Error {
        Error::Invalid(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_error(kind: io::ErrorKind) -> String {
        Error::Read {
            path: PathBuf::from("/etc/raycat/config.toml"),
            source: io::Error::from(kind),
        }
        .to_string()
    }

    #[test]
    fn a_denied_read_tells_what_to_do() {
        let text = read_error(io::ErrorKind::PermissionDenied);
        assert!(text.starts_with("не удалось прочитать файл настроек /etc/raycat/config.toml"));
        assert!(text.contains("chmod 644"), "{text}");
        assert!(text.contains("chown 0:0"), "{text}");
    }

    #[test]
    fn other_read_errors_have_no_hint() {
        let text = read_error(io::ErrorKind::NotFound);
        assert!(!text.contains("Подсказка"), "{text}");
    }
}
