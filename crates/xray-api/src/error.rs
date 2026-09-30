use std::error::Error;
use std::fmt;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Соединение не установилось или оборвалось, либо xray не ответил вовремя.
    Unreachable { addr: SocketAddr, reason: String },
    /// xray ответил ошибкой на запрос.
    Request {
        method: &'static str,
        details: String,
    },
    EmptyTag,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { addr, reason } => {
                write!(f, "API xray не отвечает на {addr}: {reason}")
            }
            Self::Request { method, details } => {
                write!(f, "запрос {method} к API xray не удался: {details}")
            }
            Self::EmptyTag => f.write_str("тег узла для закрепления не задан"),
        }
    }
}

impl Error for ApiError {}

/// Вся цепочка причин: у ошибок tonic в верхнем сообщении нет подробностей.
pub(crate) fn describe(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::*;

    #[test]
    fn messages_are_in_russian_and_name_the_address() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 10085));
        let error = ApiError::Unreachable {
            addr,
            reason: "connection refused".into(),
        };

        assert_eq!(
            error.to_string(),
            "API xray не отвечает на 127.0.0.1:10085: connection refused"
        );
    }

    #[test]
    fn request_error_names_the_method() {
        let error = ApiError::Request {
            method: "OverrideBalancerTarget",
            details: "cannot find tag".into(),
        };

        assert_eq!(
            error.to_string(),
            "запрос OverrideBalancerTarget к API xray не удался: cannot find tag"
        );
    }

    #[derive(Debug)]
    struct Outer(std::io::Error);

    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("transport error")
        }
    }

    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn describe_joins_the_chain_of_causes() {
        let error = Outer(std::io::Error::other("connection refused"));

        assert_eq!(describe(&error), "transport error: connection refused");
    }
}
