//! Проверка клиента с настоящим xray. Запускается, только если задана
//! переменная `XRAY_BIN` (путь к бинарнику; относительный путь считается от
//! корня репозитория).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use raycat_xray_api::{ApiError, XrayApi};
use serde_json::{Value, json};

const BALANCER: &str = "auto";
const NODES: [&str; 2] = ["node-001-main", "node-002-main"];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(40);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// Узлы указывают на закрытые порты loopback: соединение отклоняется сразу,
// проверка observatory заканчивается ошибкой без ожидания и без сети.
fn config(api_port: u16) -> Value {
    let node = |tag: &str| {
        json!({
            "tag": tag,
            "protocol": "trojan",
            "settings": {"servers": [{
                "address": "127.0.0.1",
                "port": free_port(),
                "password": "00000000-0000-0000-0000-000000000000",
            }]},
        })
    };
    json!({
        "log": {"loglevel": "warning"},
        "api": {
            "tag": "api",
            "listen": format!("127.0.0.1:{api_port}"),
            "services": ["HandlerService", "RoutingService", "ObservatoryService", "StatsService"],
        },
        "stats": {},
        "policy": {"system": {"statsOutboundUplink": true, "statsOutboundDownlink": true}},
        "observatory": {
            "subjectSelector": NODES,
            "probeUrl": "https://www.gstatic.com/generate_204",
            "probeInterval": "1s",
            "enableConcurrency": true,
        },
        "routing": {
            "balancers": [{
                "tag": BALANCER,
                "selector": NODES,
                "strategy": {"type": "leastPing"},
                "fallbackTag": NODES[0],
            }],
            "rules": [
                {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
                {"type": "field", "network": "tcp,udp", "balancerTag": BALANCER},
            ],
        },
        "outbounds": [node(NODES[0]), node(NODES[1])],
    })
}

struct RunningXray {
    child: Child,
    dir: PathBuf,
}

impl RunningXray {
    fn start(binary: &Path, api_port: u16) -> Self {
        let dir = std::env::temp_dir().join(format!("raycat-xray-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&config(api_port)).unwrap()).unwrap();

        let child = Command::new(binary)
            .args(["run", "-c"])
            .arg(&path)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, dir }
    }
}

impl Drop for RunningXray {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn xray_binary() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("XRAY_BIN")?);
    if path.is_absolute() {
        return Some(path);
    }
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path))
}

async fn connect(xray: &mut RunningXray, api_port: u16) -> XrayApi {
    let started = Instant::now();
    loop {
        if let Some(status) = xray.child.try_wait().unwrap() {
            panic!("xray завершился при запуске: {status}");
        }
        match XrayApi::connect(api_port, Duration::from_secs(2)).await {
            Ok(api) => return api,
            Err(error) if started.elapsed() > STARTUP_TIMEOUT => {
                panic!("API xray не поднялось за {STARTUP_TIMEOUT:?}: {error}")
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn check_pinning(api: &XrayApi) {
    let info = api.balancer_info(BALANCER).await.unwrap();
    assert_eq!(info.pinned, None);

    api.pin(BALANCER, NODES[1]).await.unwrap();
    let info = api.balancer_info(BALANCER).await.unwrap();
    assert_eq!(info.pinned.as_deref(), Some(NODES[1]));

    api.unpin(BALANCER).await.unwrap();
    let info = api.balancer_info(BALANCER).await.unwrap();
    assert_eq!(info.pinned, None);

    assert_eq!(api.pin(BALANCER, "").await, Err(ApiError::EmptyTag));
    let error = api.pin("no-such-balancer", NODES[0]).await.unwrap_err();
    assert!(
        matches!(error, ApiError::Request { method: "OverrideBalancerTarget", .. }),
        "{error}"
    );
}

async fn check_health(api: &XrayApi) {
    let started = Instant::now();
    let nodes = loop {
        let nodes = api.outbound_status().await.unwrap();
        if nodes.len() == NODES.len() && nodes.iter().all(|node| node.last_try.is_some()) {
            break nodes;
        }
        assert!(
            started.elapsed() < PROBE_TIMEOUT,
            "observatory не проверил узлы за {PROBE_TIMEOUT:?}: {nodes:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    let tags: Vec<&str> = nodes.iter().map(|node| node.tag.as_str()).collect();
    assert_eq!(tags, NODES);
    for node in &nodes {
        assert!(!node.alive, "{node:?}");
        assert_eq!(node.delay, None, "{node:?}");
        assert!(
            node.last_error.as_deref().is_some_and(|text| !text.is_empty()),
            "{node:?}"
        );
    }
}

async fn check_traffic(api: &XrayApi) {
    let traffic = api.outbound_traffic().await.unwrap();
    let tags: Vec<&str> = traffic.iter().map(|entry| entry.tag.as_str()).collect();
    for node in NODES {
        assert!(tags.contains(&node), "нет счётчиков {node}: {traffic:?}");
    }
}

#[tokio::test]
async fn client_works_against_real_xray() {
    let Some(binary) = xray_binary() else {
        eprintln!("XRAY_BIN не задан, проверка с настоящим xray пропущена");
        return;
    };
    let api_port = free_port();
    let mut xray = RunningXray::start(&binary, api_port);

    let api = connect(&mut xray, api_port).await;
    check_pinning(&api).await;
    check_health(&api).await;
    check_traffic(&api).await;
}

#[tokio::test]
async fn closed_port_is_reported_in_russian() {
    let port = free_port();

    let error = XrayApi::connect(port, Duration::from_secs(2))
        .await
        .unwrap_err();

    assert!(
        matches!(error, ApiError::Unreachable { .. }),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .starts_with(&format!("API xray не отвечает на 127.0.0.1:{port}")),
        "{error}"
    );
}
