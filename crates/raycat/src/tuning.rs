//! Настройки производительности, зависящие от хоста: управление перегрузкой TCP и
//! буферы UDP для QUIC. Разбор `/proc` отделён от чтения файлов, чтобы его можно
//! было проверять на тексте.

use std::fs;

use raycat_config::TcpCongestion;

const AVAILABLE_CONGESTION: &str = "/proc/sys/net/ipv4/tcp_available_congestion_control";
const ALLOWED_CONGESTION: &str = "/proc/sys/net/ipv4/tcp_allowed_congestion_control";
const PROCESS_STATUS: &str = "/proc/self/status";
const RMEM_MAX: &str = "/proc/sys/net/core/rmem_max";
const WMEM_MAX: &str = "/proc/sys/net/core/wmem_max";

const AUTO_ALGORITHM: &str = "bbr";
const CAP_NET_ADMIN: u32 = 12;
const QUIC_BUFFER_BYTES: u64 = 7_864_320;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Off,
    Use(String),
    /// `auto` не стал включать алгоритм; причина для лога.
    Skipped(String),
}

impl Decision {
    pub(crate) fn algorithm(&self) -> Option<&str> {
        match self {
            Self::Use(name) => Some(name),
            Self::Off | Self::Skipped(_) => None,
        }
    }

    /// Строка для лога; для явного `off` ничего не говорится.
    pub(crate) fn describe(&self) -> Option<String> {
        match self {
            Self::Off => None,
            Self::Use(name) => Some(format!(
                "TCP: для соединений к узлам включено управление перегрузкой {name}"
            )),
            Self::Skipped(reason) => Some(format!("TCP: {AUTO_ALGORITHM} не включён ({reason})")),
        }
    }
}

/// Решение по настройке с учётом текущего хоста.
pub(crate) fn congestion(setting: &TcpCongestion) -> Decision {
    match setting {
        TcpCongestion::Off => Decision::Off,
        TcpCongestion::Algorithm(name) => Decision::Use(name.clone()),
        TcpCongestion::Auto => {
            let read = |path: &str| fs::read_to_string(path).ok();
            decide_auto(
                read(AVAILABLE_CONGESTION).as_deref(),
                read(ALLOWED_CONGESTION).as_deref(),
                read(PROCESS_STATUS).as_deref(),
            )
        }
    }
}

/// `auto` включает bbr, только когда `setsockopt(TCP_CONGESTION)` у xray заведомо
/// удастся: при неудаче xray лишь пишет строку в лог на уровне info и продолжает
/// соединение без bbr, но пропускает и остальные опции сокета, которые идут после
/// неё. Поэтому нужны оба условия: алгоритм загружен в ядро (из контейнера его не
/// загрузить) и либо разрешён всем процессам, либо у xray есть `CAP_NET_ADMIN`.
fn decide_auto(available: Option<&str>, allowed: Option<&str>, status: Option<&str>) -> Decision {
    let (Some(available), Some(allowed)) = (available, allowed) else {
        return Decision::Skipped("не удалось прочитать настройки ядра в /proc/sys/net".to_owned());
    };
    if !lists(available, AUTO_ALGORITHM) {
        return Decision::Skipped(format!(
            "ядро его не поддерживает: нет в tcp_available_congestion_control, \
             на хосте нужен модуль tcp_{AUTO_ALGORITHM}"
        ));
    }
    if lists(allowed, AUTO_ALGORITHM) || status.is_some_and(child_has_net_admin) {
        return Decision::Use(AUTO_ALGORITHM.to_owned());
    }
    Decision::Skipped(
        "он не разрешён в tcp_allowed_congestion_control, а у процесса нет CAP_NET_ADMIN"
            .to_owned(),
    )
}

fn lists(list: &str, name: &str) -> bool {
    list.split_whitespace().any(|item| item == name)
}

/// Получит ли `CAP_NET_ADMIN` запускаемый демоном xray: она нужна и самому демону
/// (`CapEff`), и должна пережить `exec`: либо в ambient-наборе, либо демон работает
/// от root, и тогда дочерний процесс получает всё, что осталось в bounding-наборе.
fn child_has_net_admin(status: &str) -> bool {
    let mask = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
            .is_some_and(|mask| mask & (1 << CAP_NET_ADMIN) != 0)
    };
    let root = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|value| value.split_whitespace().nth(1))
        == Some("0");
    mask("CapEff:") && (mask("CapAmb:") || (root && mask("CapBnd:")))
}

/// Предупреждение о малых буферах UDP; `None`, если буферы достаточны или их не
/// удалось прочитать.
pub(crate) fn udp_buffers_warning() -> Option<String> {
    let read = |path: &str| fs::read_to_string(path).ok();
    udp_buffers_message(read(RMEM_MAX).as_deref(), read(WMEM_MAX).as_deref())
}

fn udp_buffers_message(rmem_max: Option<&str>, wmem_max: Option<&str>) -> Option<String> {
    let smallest = [rmem_max, wmem_max]
        .into_iter()
        .flatten()
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .min()?;
    (smallest < QUIC_BUFFER_BYTES).then(|| {
        format!(
            "буфер UDP {} КиБ, QUIC может работать медленно; увеличьте net.core.rmem_max и net.core.wmem_max до 7.5 МиБ на хосте",
            smallest / 1024
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_WITH_CAP: &str = "Name:\traycat\nUid:\t0\t0\t0\t0\nCapInh:\t0000000000000000\nCapPrm:\t000001ffffffffff\nCapEff:\t000001ffffffffff\nCapBnd:\t000001ffffffffff\nCapAmb:\t0000000000000000\n";
    const USER_WITH_AMBIENT: &str = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000001000\nCapBnd:\t0000000000001000\nCapAmb:\t0000000000001000\n";
    const USER_WITHOUT_AMBIENT: &str = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000001000\nCapBnd:\t0000000000001000\nCapAmb:\t0000000000000000\n";
    const ROOT_WITHOUT_CAP: &str = "Uid:\t0\t0\t0\t0\nCapEff:\t00000000a80425fb\nCapBnd:\t00000000a80425fb\nCapAmb:\t0000000000000000\n";
    const ROOT_BOUNDING_DROPPED: &str = "Uid:\t0\t0\t0\t0\nCapEff:\t0000000000001000\nCapBnd:\t0000000000000000\nCapAmb:\t0000000000000000\n";
    const UNPRIVILEGED: &str = "Uid:\t1000\t1000\t1000\t1000\nCapEff:\t0000000000000000\nCapBnd:\t000001ffffffffff\nCapAmb:\t0000000000000000\n";

    fn auto(available: &str, allowed: &str, status: &str) -> Decision {
        decide_auto(Some(available), Some(allowed), Some(status))
    }

    #[test]
    fn bbr_allowed_for_everyone_is_enabled() {
        let decision = auto("reno cubic bbr", "reno cubic bbr", UNPRIVILEGED);
        assert_eq!(decision, Decision::Use("bbr".to_owned()));
        assert_eq!(decision.algorithm(), Some("bbr"));
    }

    #[test]
    fn available_but_not_allowed_needs_net_admin() {
        let available = "reno cubic bbr";
        let allowed = "reno cubic";
        assert_eq!(
            auto(available, allowed, ROOT_WITH_CAP).algorithm(),
            Some("bbr")
        );
        assert_eq!(
            auto(available, allowed, USER_WITH_AMBIENT).algorithm(),
            Some("bbr")
        );
        for status in [
            USER_WITHOUT_AMBIENT,
            ROOT_WITHOUT_CAP,
            ROOT_BOUNDING_DROPPED,
            UNPRIVILEGED,
            "",
        ] {
            assert!(
                matches!(auto(available, allowed, status), Decision::Skipped(_)),
                "{status}"
            );
        }
    }

    #[test]
    fn unavailable_bbr_is_never_enabled() {
        for status in [ROOT_WITH_CAP, UNPRIVILEGED] {
            let decision = auto("reno cubic", "reno cubic bbr", status);
            assert!(matches!(&decision, Decision::Skipped(reason) if reason.contains("tcp_bbr")));
            assert_eq!(decision.algorithm(), None);
        }
    }

    #[test]
    fn names_are_matched_as_whole_words() {
        let decision = auto("reno cubic bbr2", "reno cubic bbr2", ROOT_WITH_CAP);
        assert!(matches!(decision, Decision::Skipped(_)));
        assert!(lists("reno\tcubic\nbbr\n", "bbr"));
        assert!(!lists("", "bbr"));
    }

    #[test]
    fn unreadable_files_mean_no_bbr() {
        assert!(matches!(
            decide_auto(None, Some("bbr"), Some(ROOT_WITH_CAP)),
            Decision::Skipped(_)
        ));
        assert!(matches!(
            decide_auto(Some("bbr"), None, Some(ROOT_WITH_CAP)),
            Decision::Skipped(_)
        ));
        assert!(matches!(
            decide_auto(Some("bbr"), Some("cubic"), None),
            Decision::Skipped(_)
        ));
        assert_eq!(
            decide_auto(Some("bbr"), Some("bbr"), None).algorithm(),
            Some("bbr")
        );
    }

    #[test]
    fn capability_parsing_ignores_garbage() {
        assert!(!child_has_net_admin(""));
        assert!(!child_has_net_admin("CapEff:\tnot-hex\nCapAmb:\t1000\n"));
        assert!(!child_has_net_admin(
            "Uid:\tx\nCapEff:\t1000\nCapBnd:\t1000\n"
        ));
        assert!(child_has_net_admin("CapEff:\t1000\nCapAmb:\t1000\n"));
    }

    #[test]
    fn explicit_settings_do_not_depend_on_the_host() {
        assert_eq!(congestion(&TcpCongestion::Off), Decision::Off);
        assert_eq!(
            congestion(&TcpCongestion::Algorithm("cubic".to_owned())),
            Decision::Use("cubic".to_owned())
        );
    }

    #[test]
    fn decisions_are_described_for_the_log() {
        assert_eq!(Decision::Off.describe(), None);
        let used = Decision::Use("bbr".to_owned()).describe().unwrap();
        assert!(used.contains("bbr"));
        let skipped = Decision::Skipped("причина".to_owned()).describe().unwrap();
        assert!(skipped.contains("не включён") && skipped.contains("причина"));
    }

    #[test]
    fn small_udp_buffers_are_reported_in_kib() {
        let message = udp_buffers_message(Some("212992\n"), Some("212992\n")).unwrap();
        assert_eq!(
            message,
            "буфер UDP 208 КиБ, QUIC может работать медленно; увеличьте net.core.rmem_max и net.core.wmem_max до 7.5 МиБ на хосте"
        );
    }

    #[test]
    fn the_smaller_of_the_two_buffers_decides() {
        let message = udp_buffers_message(Some("8000000\n"), Some("1048576\n")).unwrap();
        assert!(message.starts_with("буфер UDP 1024 КиБ"));
        let message = udp_buffers_message(Some("1048576\n"), Some("8000000\n")).unwrap();
        assert!(message.starts_with("буфер UDP 1024 КиБ"));
    }

    #[test]
    fn large_or_unreadable_udp_buffers_are_silent() {
        assert_eq!(udp_buffers_message(Some("7864320"), Some("7864320")), None);
        assert_eq!(
            udp_buffers_message(Some("26214400"), Some("26214400")),
            None
        );
        assert_eq!(udp_buffers_message(None, None), None);
        assert_eq!(udp_buffers_message(Some("many"), Some("")), None);
    }

    #[test]
    fn one_readable_buffer_is_enough_to_warn() {
        assert!(udp_buffers_message(Some("212992"), None).is_some());
        assert!(udp_buffers_message(None, Some("212992")).is_some());
    }
}
