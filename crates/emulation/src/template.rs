//! Шаблоны значений заголовков: текст с подстановками `{имя}`.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Var {
    Host,
    UserAgent,
    AppVersion,
    Build,
    Tail,
    Marker,
    Cpu,
    Os,
    OsVersion,
    Model,
    Hostname,
    Hwid,
    DeviceLocale,
    AcceptLanguage,
}

impl Var {
    const NAMES: &'static [(&'static str, Var)] = &[
        ("host", Var::Host),
        ("user_agent", Var::UserAgent),
        ("app_version", Var::AppVersion),
        ("build", Var::Build),
        ("tail", Var::Tail),
        ("marker", Var::Marker),
        ("cpu", Var::Cpu),
        ("os", Var::Os),
        ("os_version", Var::OsVersion),
        ("model", Var::Model),
        ("hostname", Var::Hostname),
        ("hwid", Var::Hwid),
        ("device_locale", Var::DeviceLocale),
        ("accept_language", Var::AcceptLanguage),
    ];

    fn from_name(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find_map(|(known, var)| (*known == name).then_some(*var))
    }

    pub(crate) fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find_map(|(name, var)| (*var == self).then_some(*name))
            .unwrap_or_default()
    }

    fn known_names() -> String {
        let names: Vec<&str> = Self::NAMES.iter().map(|(name, _)| *name).collect();
        names.join(", ")
    }
}

/// Значения подстановок для одного запроса.
pub(crate) struct Values<'a> {
    pub(crate) host: &'a str,
    pub(crate) user_agent: &'a str,
    pub(crate) app_version: &'a str,
    pub(crate) build: &'a str,
    pub(crate) tail: &'a str,
    pub(crate) marker: &'a str,
    pub(crate) cpu: &'a str,
    pub(crate) os: &'a str,
    pub(crate) os_version: &'a str,
    pub(crate) model: &'a str,
    pub(crate) hostname: &'a str,
    pub(crate) hwid: &'a str,
    pub(crate) device_locale: &'a str,
    pub(crate) accept_language: &'a str,
}

impl Values<'_> {
    fn get(&self, var: Var) -> &str {
        match var {
            Var::Host => self.host,
            Var::UserAgent => self.user_agent,
            Var::AppVersion => self.app_version,
            Var::Build => self.build,
            Var::Tail => self.tail,
            Var::Marker => self.marker,
            Var::Cpu => self.cpu,
            Var::Os => self.os,
            Var::OsVersion => self.os_version,
            Var::Model => self.model,
            Var::Hostname => self.hostname,
            Var::Hwid => self.hwid,
            Var::DeviceLocale => self.device_locale,
            Var::AcceptLanguage => self.accept_language,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Var(Var),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Template(Vec<Part>);

impl Template {
    pub(crate) fn parse(source: &str) -> Result<Self> {
        let mut parts = Vec::new();
        let mut rest = source;
        while !rest.is_empty() {
            let Some(at) = rest.find(['{', '}']) else {
                parts.push(Part::Text(rest.to_owned()));
                break;
            };
            if rest[at..].starts_with('}') {
                bail!("лишняя закрывающая скобка }}");
            }
            if at > 0 {
                parts.push(Part::Text(rest[..at].to_owned()));
            }
            let after = &rest[at + 1..];
            let Some(end) = after.find('}') else {
                bail!("нет закрывающей скобки }}");
            };
            let name = &after[..end];
            let Some(var) = Var::from_name(name) else {
                bail!(
                    "неизвестная подстановка {{{name}}}, доступны: {}",
                    Var::known_names()
                );
            };
            parts.push(Part::Var(var));
            rest = &after[end + 1..];
        }
        Ok(Self(parts))
    }

    pub(crate) fn vars(&self) -> impl Iterator<Item = Var> + '_ {
        self.0.iter().filter_map(|part| match part {
            Part::Var(var) => Some(*var),
            Part::Text(_) => None,
        })
    }

    pub(crate) fn render(&self, values: &Values<'_>) -> String {
        let mut out = String::new();
        for part in &self.0 {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Var(var) => out.push_str(values.get(*var)),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values<'static> {
        Values {
            host: "example.com",
            user_agent: "UA",
            app_version: "1.2.3",
            build: "100",
            tail: "07",
            marker: "5",
            cpu: "arm64",
            os: "Windows",
            os_version: "11",
            model: "PC",
            hostname: "HOST",
            hwid: "ID",
            device_locale: "EN",
            accept_language: "en-US,*",
        }
    }

    #[test]
    fn renders_text_and_substitutions() {
        let template = Template::parse("Happ/{app_version}/{os}/{build}{marker}{tail}").unwrap();
        assert_eq!(template.render(&values()), "Happ/1.2.3/Windows/100507");
        assert_eq!(Template::parse("gzip, deflate").unwrap().render(&values()), "gzip, deflate");
        assert_eq!(Template::parse("").unwrap().render(&values()), "");
        assert_eq!(Template::parse("{host}").unwrap().render(&values()), "example.com");
    }

    #[test]
    fn every_known_name_is_substituted() {
        for (name, var) in Var::NAMES {
            let template = Template::parse(&format!("{{{name}}}")).unwrap();
            assert_eq!(template.vars().collect::<Vec<_>>(), [*var]);
            assert!(!template.render(&values()).is_empty(), "{name}");
        }
    }

    #[test]
    fn rejects_unknown_and_broken_substitutions() {
        let unknown = Template::parse("{nope}").unwrap_err().to_string();
        assert!(unknown.contains("{nope}"), "{unknown}");
        assert!(Template::parse("{host").is_err());
        assert!(Template::parse("host}").is_err());
        assert!(Template::parse("{}").is_err());
        assert!(Template::parse("{ho{st}}").is_err());
        assert!(Template::parse("{Host}").is_err());
    }
}
