//! Команды, которые говорят с работающим демоном: `status`, `health`, `nodes`, `use`,
//! `update`, `events`. Настройки и каталог состояния им не нужны: только сокет.

use std::fmt;
use std::io::{self, Write as _};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use raycat_config::Env;
use raycat_proto::{Node, NodeStatus};
use serde::Serialize;

use crate::client::Client;
use crate::paths;
use crate::render;
use crate::term::{Term, Tone};
use crate::util::{local_zone, now_unix, sanitize};

const MAX_LISTED: usize = 10;
/// Меньше `timeout` в HEALTHCHECK образа: ответ о неготовности должен прийти раньше,
/// чем Docker убьёт проверку.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// Строка в stdout; закрытый канал (`| head`) не повод падать.
fn out(text: &str) {
    let _ = writeln!(io::stdout().lock(), "{text}");
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("не удалось собрать JSON")
}

#[derive(Debug, PartialEq, Eq)]
enum ResolveError {
    NotFound(String),
    Ambiguous { query: String, ids: Vec<String> },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(query) => write!(
                f,
                "узла «{}» нет среди узлов подписок (список: raycat nodes --all)",
                sanitize(query)
            ),
            Self::Ambiguous { query, ids } => {
                write!(
                    f,
                    "«{}» подходит к нескольким узлам, уточните запрос:",
                    sanitize(query)
                )?;
                for id in ids.iter().take(MAX_LISTED) {
                    write!(f, "\n  {}", sanitize(id))?;
                }
                if ids.len() > MAX_LISTED {
                    write!(f, "\n  … и ещё {}", ids.len() - MAX_LISTED)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Ищет узел по запросу без учёта регистра: полное «подписка/имя», затем полное
/// имя, затем часть «подписка/имя». Первая стадия с совпадениями решает; если на
/// ней больше одного узла — запрос неоднозначен.
fn resolve<'a>(nodes: &'a [Node], query: &str) -> Result<&'a Node, ResolveError> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err(ResolveError::NotFound(query.to_owned()));
    }
    let stages: [&dyn Fn(&Node) -> bool; 3] = [
        &|node| node.id.to_lowercase() == needle,
        &|node| node.name.to_lowercase() == needle,
        &|node| node.id.to_lowercase().contains(&needle),
    ];
    for stage in stages {
        let found: Vec<&Node> = nodes.iter().filter(|node| stage(node)).collect();
        match found.as_slice() {
            [] => {}
            [node] => return Ok(*node),
            many => {
                return Err(ResolveError::Ambiguous {
                    query: query.to_owned(),
                    ids: many.iter().map(|node| node.id.clone()).collect(),
                });
            }
        }
    }
    Err(ResolveError::NotFound(query.to_owned()))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("не удалось запустить среду выполнения")
}

pub(crate) fn run(name: &str, sub: &ArgMatches, env: &Env) -> Result<()> {
    let client = Client::new(paths::client_socket(env, paths::is_root())?);
    let json = sub.get_flag("json");
    let term = if json {
        Term::new(false, None)
    } else {
        Term::detect(env)
    };
    runtime()?.block_on(dispatch(name, sub, &client, term, json))
}

/// `raycat health`: проверка для HEALTHCHECK. Ничего не создаёт и отвечает быстро.
pub(crate) fn health(env: &Env) -> Result<()> {
    let socket = paths::client_socket(env, paths::is_root())?;
    let client = Client::with_timeout(socket, HEALTH_TIMEOUT);
    out(&runtime()?.block_on(ready(&client))?);
    Ok(())
}

/// Демон отвечает, xray работает и узел выбран. xray запускается только после установки
/// правил шлюза, поэтому отдельного признака для них не нужно.
async fn ready(client: &Client) -> Result<String> {
    let status = client.status().await?;
    if !status.xray.running {
        bail!("xray не запущен; подробности: raycat status");
    }
    let Some(node) = &status.node else {
        bail!("узел не выбран: подписки ещё не дали рабочих узлов; подробности: raycat status");
    };
    Ok(format!("готов: xray работает, узел {}", sanitize(&node.id)))
}

async fn dispatch(
    name: &str,
    sub: &ArgMatches,
    client: &Client,
    term: Term,
    json: bool,
) -> Result<()> {
    match name {
        "status" => status(client, term, json).await,
        "nodes" => {
            let subscription = sub.get_one::<String>("subscription").map(String::as_str);
            nodes(client, term, json, sub.get_flag("all"), subscription).await
        }
        "use" => {
            let query = sub.get_one::<String>("node").context("не указан узел")?;
            use_node(client, term, json, query).await
        }
        "update" => {
            let subscription = sub.get_one::<String>("subscription").map(String::as_str);
            update(client, term, json, subscription).await
        }
        "events" => events(client, term, json).await,
        other => bail!("неизвестная команда {other}"),
    }
}

async fn status(client: &Client, term: Term, json: bool) -> Result<()> {
    let status = client.status().await?;
    if json {
        out(&to_json(&status)?);
    } else {
        out(&render::status(term, &status, now_unix(), local_zone()));
    }
    Ok(())
}

async fn nodes(
    client: &Client,
    term: Term,
    json: bool,
    all: bool,
    subscription: Option<&str>,
) -> Result<()> {
    let mut nodes = client.nodes().await?;
    if let Some(wanted) = subscription {
        let names: Vec<String> = client
            .status()
            .await?
            .subscriptions
            .into_iter()
            .map(|sub| sub.name)
            .collect();
        nodes.nodes = subscription_nodes(&names, nodes.nodes, wanted)?;
    }
    if json {
        out(&to_json(&nodes)?);
    } else {
        out(&render::nodes(term, &nodes, all));
    }
    Ok(())
}

/// Узлы одной подписки. Имя ищется без учёта регистра; при неизвестном выводятся известные.
fn subscription_nodes(names: &[String], nodes: Vec<Node>, wanted: &str) -> Result<Vec<Node>> {
    let name = subscription_name(names, wanted)?;
    Ok(nodes
        .into_iter()
        .filter(|node| node.subscription == name)
        .collect())
}

fn subscription_name(names: &[String], wanted: &str) -> Result<String> {
    let needle = wanted.trim().to_lowercase();
    if let Some(name) = names.iter().find(|name| name.to_lowercase() == needle) {
        return Ok(name.clone());
    }
    if names.is_empty() {
        bail!(
            "подписки «{}» нет: подписок ещё нет (см. raycat status)",
            sanitize(wanted)
        );
    }
    let known: Vec<String> = names.iter().map(|name| sanitize(name)).collect();
    bail!(
        "подписки «{}» нет; есть: {}",
        sanitize(wanted),
        known.join(", ")
    )
}

async fn use_node(client: &Client, term: Term, json: bool, query: &str) -> Result<()> {
    let mut unresponsive = false;
    let pinned = if query.trim().eq_ignore_ascii_case("auto") {
        client.unpin().await?
    } else {
        let nodes = client.nodes().await?;
        let node = resolve(&nodes.nodes, query)?;
        unresponsive = node.status == NodeStatus::Dead;
        client.pin(&node.id).await?
    };
    if json {
        out(&to_json(&pinned)?);
        return Ok(());
    }
    match &pinned.node {
        Some(id) => {
            out(&format!(
                "Узел «{}» закреплён. Вернуть автоматический выбор: raycat use auto",
                sanitize(id)
            ));
            if unresponsive {
                out(&term.paint(Tone::Yellow, "Внимание: сейчас этот узел не отвечает."));
            }
        }
        None => out("Закрепление снято: узел выбирается автоматически"),
    }
    Ok(())
}

async fn update(client: &Client, term: Term, json: bool, subscription: Option<&str>) -> Result<()> {
    let updates = client.update(subscription).await?;
    if json {
        out(&to_json(&updates)?);
    } else {
        out(&render::updates(term, &updates));
    }
    let failed = render::failed(&updates);
    if failed > 0 {
        bail!("{}", not_updated(failed, updates.results.len()));
    }
    Ok(())
}

/// «подписка» для 1, 21, 101; «подписки» для 2–4, 22–24; «подписок» в остальных случаях,
/// включая 11–14.
fn subscriptions_word(count: usize) -> &'static str {
    let last = count % 10;
    let tens = count % 100;
    if last == 1 && tens != 11 {
        "подписка"
    } else if (2..=4).contains(&last) && !(12..=14).contains(&tens) {
        "подписки"
    } else {
        "подписок"
    }
}

fn not_updated(failed: usize, total: usize) -> String {
    let verb = match subscriptions_word(failed) {
        "подписка" => "обновилась",
        _ => "обновились",
    };
    format!(
        "не {verb} {failed} {} из {total}",
        subscriptions_word(failed)
    )
}

async fn events(client: &Client, term: Term, json: bool) -> Result<()> {
    let mut stream = client.events().await?;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        tokio::select! {
            _ = &mut interrupt => return Ok(()),
            next = stream.next() => {
                let Some(event) = next? else {
                    bail!("демон закрыл поток событий: возможно, он остановлен");
                };
                if json {
                    out(&serde_json::to_string(&event).context("не удалось собрать JSON")?);
                } else {
                    let now = now_unix();
                    out(&render::event_line(term, now, now, local_zone(), &event));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(subscription: &str, name: &str) -> Node {
        Node {
            id: format!("{subscription}/{name}"),
            subscription: subscription.to_owned(),
            name: name.to_owned(),
            tag: "node-001".to_owned(),
            status: NodeStatus::Alive,
            latency_ms: Some(10),
            failures: 0,
            alive_for_secs: None,
            last_error: None,
            selected: false,
            pinned: false,
            uplink_bytes: None,
            downlink_bytes: None,
        }
    }

    fn sample() -> Vec<Node> {
        vec![
            node("main", "NL-1"),
            node("main", "NL-10"),
            node("main", "Германия 1"),
            node("backup", "NL-1"),
            node("backup", "Финляндия"),
        ]
    }

    #[test]
    fn the_full_id_picks_exactly_one_node() {
        let nodes = sample();
        assert_eq!(resolve(&nodes, "main/NL-1").unwrap().id, "main/NL-1");
        assert_eq!(resolve(&nodes, "BACKUP/nl-1").unwrap().id, "backup/NL-1");
    }

    #[test]
    fn an_exact_name_beats_a_longer_name_that_contains_it() {
        let nodes = vec![node("main", "NL-1"), node("main", "NL-10")];
        assert_eq!(resolve(&nodes, "nl-1").unwrap().id, "main/NL-1");
    }

    #[test]
    fn a_unique_part_of_the_name_is_enough_in_any_case() {
        let nodes = sample();
        assert_eq!(resolve(&nodes, "ГЕРМАН").unwrap().id, "main/Германия 1");
        assert_eq!(resolve(&nodes, "финл").unwrap().id, "backup/Финляндия");
        assert_eq!(resolve(&nodes, "  финл  ").unwrap().id, "backup/Финляндия");
    }

    #[test]
    fn the_same_name_in_two_subscriptions_is_ambiguous() {
        let nodes = sample();
        let error = resolve(&nodes, "NL-1").unwrap_err();
        assert_eq!(
            error,
            ResolveError::Ambiguous {
                query: "NL-1".to_owned(),
                ids: vec!["main/NL-1".to_owned(), "backup/NL-1".to_owned()],
            }
        );
        let text = error.to_string();
        assert!(text.contains("подходит к нескольким"));
        assert!(text.contains("\n  main/NL-1\n  backup/NL-1"));
    }

    #[test]
    fn a_part_that_matches_several_nodes_lists_them() {
        let nodes = sample();
        let error = resolve(&nodes, "nl").unwrap_err();
        let ResolveError::Ambiguous { ids, .. } = error else {
            panic!("ожидалась неоднозначность");
        };
        assert_eq!(ids.len(), 3);
        let by_subscription = resolve(&nodes, "backup").unwrap_err();
        assert!(
            matches!(by_subscription, ResolveError::Ambiguous { ref ids, .. } if ids.len() == 2)
        );
    }

    #[test]
    fn a_long_list_of_candidates_is_cut() {
        let nodes: Vec<Node> = (0..15)
            .map(|index| node("main", &format!("DE-{index:02}")))
            .collect();
        let text = resolve(&nodes, "de").unwrap_err().to_string();
        assert_eq!(text.lines().count(), 1 + MAX_LISTED + 1);
        assert!(text.ends_with("… и ещё 5"));
    }

    #[test]
    fn an_unknown_node_points_to_the_list() {
        let nodes = sample();
        let error = resolve(&nodes, "Токио").unwrap_err();
        assert_eq!(error, ResolveError::NotFound("Токио".to_owned()));
        assert!(error.to_string().contains("raycat nodes --all"));
        assert!(matches!(
            resolve(&nodes, "  ").unwrap_err(),
            ResolveError::NotFound(_)
        ));
        assert!(matches!(
            resolve(&[], "x").unwrap_err(),
            ResolveError::NotFound(_)
        ));
    }

    #[test]
    fn the_query_cannot_inject_terminal_codes_into_the_error() {
        let error = resolve(&sample(), "\x1b[2J").unwrap_err();
        assert!(!error.to_string().contains('\x1b'));
    }

    #[test]
    fn a_subscription_keeps_only_its_nodes() {
        let names = vec!["main".to_owned(), "backup".to_owned()];
        let kept = subscription_nodes(&names, sample(), "BACKUP").unwrap();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|node| node.subscription == "backup"));
    }

    #[test]
    fn an_unknown_subscription_lists_the_known_ones() {
        let names = vec!["main".to_owned(), "backup".to_owned()];
        let error = subscription_nodes(&names, sample(), "Токио").unwrap_err();
        assert_eq!(
            error.to_string(),
            "подписки «Токио» нет; есть: main, backup"
        );
        let none = subscription_name(&[], "main").unwrap_err();
        assert_eq!(
            none.to_string(),
            "подписки «main» нет: подписок ещё нет (см. raycat status)"
        );
    }

    #[test]
    fn the_subscription_name_cannot_inject_terminal_codes() {
        let names = vec!["main".to_owned()];
        let error = subscription_name(&names, "\x1b[2J").unwrap_err();
        assert!(!error.to_string().contains('\x1b'));
    }

    #[test]
    fn the_word_for_subscriptions_follows_the_count() {
        for count in [1, 21, 101] {
            assert_eq!(subscriptions_word(count), "подписка", "{count}");
        }
        for count in [2, 3, 4, 22, 24, 102] {
            assert_eq!(subscriptions_word(count), "подписки", "{count}");
        }
        for count in [0, 5, 11, 12, 14, 20, 25, 111, 112] {
            assert_eq!(subscriptions_word(count), "подписок", "{count}");
        }
    }

    #[test]
    fn the_update_failure_agrees_with_the_count() {
        assert_eq!(not_updated(1, 1), "не обновилась 1 подписка из 1");
        assert_eq!(not_updated(2, 3), "не обновились 2 подписки из 3");
        assert_eq!(not_updated(5, 7), "не обновились 5 подписок из 7");
        assert_eq!(not_updated(11, 12), "не обновились 11 подписок из 12");
        assert_eq!(not_updated(21, 22), "не обновилась 21 подписка из 22");
    }
}

#[cfg(test)]
mod health_tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::UnixListener;

    use super::*;
    use crate::testing::{TempDir, http_response};

    const NODE: &str = r#"{"id":"main/NL-1","subscription":"main","name":"NL-1","latency_ms":31,"pinned":false,"reason":null}"#;

    fn status_json(running: bool, node: Option<&str>) -> String {
        format!(
            r#"{{"version":"0.1.0","mode":"gateway","uptime_secs":5,"kill_switch":true,
            "xray":{{"running":{running},"pid":null,"restarts":0}},
            "node":{},"subscriptions":[]}}"#,
            node.unwrap_or("null")
        )
    }

    struct Fake {
        _temp: TempDir,
        socket: PathBuf,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn fake(reply: String) -> Fake {
        let temp = TempDir::new("health");
        let socket = temp.path().join("raycat.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut raw = Vec::new();
                let mut byte = [0u8; 1];
                while !raw.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                        break;
                    }
                    raw.push(byte[0]);
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).into_owned());
                let response =
                    http_response("200 OK", &[("Content-Type", "application/json")], &reply);
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Fake {
            _temp: temp,
            socket,
            requests,
        }
    }

    #[tokio::test]
    async fn a_running_xray_with_a_node_is_ready() {
        let fake = fake(status_json(true, Some(NODE)));
        let line = ready(&Client::new(fake.socket.clone())).await.unwrap();
        assert_eq!(line, "готов: xray работает, узел main/NL-1");
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v1/status HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn a_stopped_xray_is_not_ready() {
        let fake = fake(status_json(false, Some(NODE)));
        let error = ready(&Client::new(fake.socket.clone())).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "xray не запущен; подробности: raycat status"
        );
    }

    #[tokio::test]
    async fn without_a_node_it_is_not_ready() {
        let fake = fake(status_json(true, None));
        let error = ready(&Client::new(fake.socket.clone())).await.unwrap_err();
        assert!(error.to_string().starts_with("узел не выбран"), "{error}");
        assert!(
            error.to_string().ends_with("подробности: raycat status"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_node_name_cannot_inject_terminal_codes() {
        let node = NODE.replace("main/NL-1", "a\\u001b[2Jb");
        let fake = fake(status_json(true, Some(&node)));
        let line = ready(&Client::new(fake.socket.clone())).await.unwrap();
        assert!(!line.contains('\x1b'), "{line:?}");
    }

    #[tokio::test]
    async fn a_missing_daemon_says_how_to_start_it() {
        let temp = TempDir::new("health-missing");
        let socket = temp.path().join("none.sock");
        let error = ready(&Client::new(socket)).await.unwrap_err();
        let text = error.to_string();
        assert!(text.starts_with("raycat не запущен\n"), "{text}");
        assert!(text.contains("systemctl start raycat"), "{text}");
    }

    #[tokio::test]
    async fn a_silent_daemon_is_not_ready_in_time() {
        let temp = TempDir::new("health-silent");
        let socket = temp.path().join("raycat.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let held = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(held);
        });
        let client = Client::with_timeout(socket, Duration::from_millis(100));
        let error = ready(&client).await.unwrap_err();
        assert!(
            error.to_string().starts_with("raycat не ответил вовремя\n"),
            "{error}"
        );
    }
}
