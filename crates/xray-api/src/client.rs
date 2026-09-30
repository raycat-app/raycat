use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status};

use crate::error::{ApiError, describe};
use crate::pb::xray::app::router::command::routing_service_client::RoutingServiceClient;
use crate::pb::xray::app::router::command::{
    GetBalancerInfoRequest, OverrideBalancerTargetRequest,
};
use crate::pb::xray::app::stats::command::stats_service_client::StatsServiceClient;
use crate::pb::xray::app::stats::command::{QueryStatsRequest, Stat};
use crate::pb::xray::core::app::observatory::OutboundStatus;
use crate::pb::xray::core::app::observatory::command::GetOutboundStatusRequest;
use crate::pb::xray::core::app::observatory::command::observatory_service_client::ObservatoryServiceClient;

const OUTBOUND_COUNTER_PREFIX: &str = "outbound>>>";
const UPLINK_SUFFIX: &str = ">>>traffic>>>uplink";
const DOWNLINK_SUFFIX: &str = ">>>traffic>>>downlink";

/// Состояние узла по данным observatory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundHealth {
    pub tag: String,
    pub alive: bool,
    /// Время последней успешной проверки; `None`, если узел мёртв.
    pub delay: Option<Duration>,
    pub last_try: Option<SystemTime>,
    pub last_seen: Option<SystemTime>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BalancerInfo {
    /// Узел, закреплённый вручную.
    pub pinned: Option<String>,
    /// Теги, которые стратегия балансировщика считает основными. Пусто, если
    /// стратегия (например, `leastPing`) их не сообщает.
    pub targets: Vec<String>,
}

/// Трафик через выход xray с момента его запуска, в байтах.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundTraffic {
    pub tag: String,
    pub uplink: u64,
    pub downlink: u64,
}

/// Клиент API xray на `127.0.0.1`.
#[derive(Debug, Clone)]
pub struct XrayApi {
    channel: Channel,
    addr: SocketAddr,
}

impl XrayApi {
    /// Подключается к API на `127.0.0.1:port`. `timeout` действует и на
    /// подключение, и на каждый запрос.
    pub async fn connect(port: u16, timeout: Duration) -> Result<Self, ApiError> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let unreachable = |reason: String| ApiError::Unreachable { addr, reason };

        let endpoint = Endpoint::from_shared(format!("http://{addr}"))
            .map_err(|error| unreachable(describe(&error)))?
            .connect_timeout(timeout)
            .timeout(timeout);
        let channel = tokio::time::timeout(timeout, endpoint.connect())
            .await
            .map_err(|_| unreachable("тайм-аут подключения".into()))?
            .map_err(|error| unreachable(describe(&error)))?;
        Ok(Self { channel, addr })
    }

    /// Состояние всех узлов, за которыми следит observatory, по тегам. Узла нет
    /// в списке, пока его ни разу не проверили.
    pub async fn outbound_status(&self) -> Result<Vec<OutboundHealth>, ApiError> {
        const METHOD: &str = "GetOutboundStatus";
        let response = ObservatoryServiceClient::new(self.channel.clone())
            .get_outbound_status(GetOutboundStatusRequest {})
            .await
            .map_err(|status| self.status_error(METHOD, &status))?
            .into_inner();

        let mut nodes: Vec<OutboundHealth> = response
            .status
            .unwrap_or_default()
            .status
            .into_iter()
            .map(health)
            .collect();
        nodes.sort_by(|a, b| a.tag.cmp(&b.tag));
        Ok(nodes)
    }

    /// Закрепляет узел `tag` в балансировщике `balancer`: весь трафик идёт через
    /// него, пока закрепление не снято.
    pub async fn pin(&self, balancer: &str, tag: &str) -> Result<(), ApiError> {
        if tag.is_empty() {
            return Err(ApiError::EmptyTag);
        }
        self.override_target(balancer, tag).await
    }

    pub async fn unpin(&self, balancer: &str) -> Result<(), ApiError> {
        self.override_target(balancer, "").await
    }

    pub async fn balancer_info(&self, balancer: &str) -> Result<BalancerInfo, ApiError> {
        const METHOD: &str = "GetBalancerInfo";
        let response = RoutingServiceClient::new(self.channel.clone())
            .get_balancer_info(GetBalancerInfoRequest {
                tag: balancer.to_owned(),
            })
            .await
            .map_err(|status| self.status_error(METHOD, &status))?
            .into_inner();

        let info = response.balancer.unwrap_or_default();
        Ok(BalancerInfo {
            pinned: info
                .r#override
                .map(|pinned| pinned.target)
                .filter(|tag| !tag.is_empty()),
            targets: info.principle_target.unwrap_or_default().tag,
        })
    }

    /// Трафик по выходам, для которых в конфиге включены счётчики
    /// (`policy.system.statsOutboundUplink` и `statsOutboundDownlink`).
    pub async fn outbound_traffic(&self) -> Result<Vec<OutboundTraffic>, ApiError> {
        const METHOD: &str = "QueryStats";
        let response = StatsServiceClient::new(self.channel.clone())
            .query_stats(QueryStatsRequest {
                pattern: OUTBOUND_COUNTER_PREFIX.to_owned(),
                reset: false,
            })
            .await
            .map_err(|status| self.status_error(METHOD, &status))?
            .into_inner();
        Ok(traffic(response.stat))
    }

    async fn override_target(&self, balancer: &str, target: &str) -> Result<(), ApiError> {
        const METHOD: &str = "OverrideBalancerTarget";
        RoutingServiceClient::new(self.channel.clone())
            .override_balancer_target(OverrideBalancerTargetRequest {
                balancer_tag: balancer.to_owned(),
                target: target.to_owned(),
            })
            .await
            .map_err(|status| self.status_error(METHOD, &status))?;
        Ok(())
    }

    fn status_error(&self, method: &'static str, status: &Status) -> ApiError {
        match status.code() {
            // Тайм-аут запроса tonic сообщает кодом Cancelled.
            Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => {
                ApiError::Unreachable {
                    addr: self.addr,
                    reason: status.message().to_owned(),
                }
            }
            code => ApiError::Request {
                method,
                details: format!("{code}: {}", status.message()),
            },
        }
    }
}

fn health(status: OutboundStatus) -> OutboundHealth {
    let delay = status
        .alive
        .then(|| Duration::from_millis(u64::try_from(status.delay).unwrap_or(0)));
    OutboundHealth {
        tag: status.outbound_tag,
        alive: status.alive,
        delay,
        last_try: unix_time(status.last_try_time),
        last_seen: unix_time(status.last_seen_time),
        last_error: Some(status.last_error_reason).filter(|reason| !reason.is_empty()),
    }
}

fn unix_time(seconds: i64) -> Option<SystemTime> {
    let seconds = u64::try_from(seconds).ok().filter(|&seconds| seconds > 0)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn traffic(stats: Vec<Stat>) -> Vec<OutboundTraffic> {
    let mut by_tag: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for stat in stats {
        let Some(rest) = stat.name.strip_prefix(OUTBOUND_COUNTER_PREFIX) else {
            continue;
        };
        let bytes = u64::try_from(stat.value).unwrap_or(0);
        if let Some(tag) = rest.strip_suffix(UPLINK_SUFFIX) {
            by_tag.entry(tag.to_owned()).or_default().0 = bytes;
        } else if let Some(tag) = rest.strip_suffix(DOWNLINK_SUFFIX) {
            by_tag.entry(tag.to_owned()).or_default().1 = bytes;
        }
    }
    by_tag
        .into_iter()
        .map(|(tag, (uplink, downlink))| OutboundTraffic {
            tag,
            uplink,
            downlink,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(alive: bool, delay: i64) -> OutboundStatus {
        OutboundStatus {
            alive,
            delay,
            last_error_reason: String::new(),
            outbound_tag: "node-001-main".into(),
            last_seen_time: 1_700_000_000,
            last_try_time: 1_700_000_030,
            health_ping: None,
        }
    }

    fn stat(name: &str, value: i64) -> Stat {
        Stat {
            name: name.into(),
            value,
        }
    }

    #[test]
    fn alive_node_reports_delay_and_times() {
        let node = health(status(true, 120));

        assert_eq!(node.tag, "node-001-main");
        assert!(node.alive);
        assert_eq!(node.delay, Some(Duration::from_millis(120)));
        assert_eq!(
            node.last_seen,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
        assert_eq!(
            node.last_try,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_030))
        );
        assert_eq!(node.last_error, None);
    }

    #[test]
    fn dead_node_has_no_delay_but_keeps_the_error() {
        let mut dead = status(false, 99_999_999);
        dead.last_seen_time = 0;
        dead.last_error_reason = "connection refused".into();

        let node = health(dead);

        assert!(!node.alive);
        assert_eq!(node.delay, None);
        assert_eq!(node.last_seen, None);
        assert_eq!(node.last_error.as_deref(), Some("connection refused"));
    }

    #[test]
    fn negative_timestamps_are_ignored() {
        assert_eq!(unix_time(-1), None);
        assert_eq!(unix_time(0), None);
    }

    #[test]
    fn counters_are_grouped_by_tag_in_order() {
        let stats = vec![
            stat("outbound>>>node-002-main>>>traffic>>>downlink", 7),
            stat("outbound>>>node-001-main>>>traffic>>>uplink", 100),
            stat("outbound>>>node-001-main>>>traffic>>>downlink", 2_000),
            stat("outbound>>>node-002-main>>>traffic>>>uplink", -5),
        ];

        assert_eq!(
            traffic(stats),
            vec![
                OutboundTraffic {
                    tag: "node-001-main".into(),
                    uplink: 100,
                    downlink: 2_000
                },
                OutboundTraffic {
                    tag: "node-002-main".into(),
                    uplink: 0,
                    downlink: 7
                },
            ]
        );
    }

    #[test]
    fn foreign_counters_are_skipped() {
        let stats = vec![
            stat("inbound>>>api>>>traffic>>>uplink", 1),
            stat("outbound>>>node-001-main>>>other", 1),
            stat("user>>>a@b>>>traffic>>>uplink", 1),
        ];

        assert_eq!(traffic(stats), vec![]);
    }
}
