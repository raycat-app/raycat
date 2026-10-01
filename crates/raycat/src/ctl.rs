//! Команды, которые говорят с работающим демоном: `status`, `nodes`, `use`,
//! `update`, `events`. Настройки и каталог состояния им не нужны: только сокет.

use std::fmt;
use std::io::{self, Write as _};

use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use raycat_config::Env;
use raycat_proto::{Node, NodeStatus};
use serde::Serialize;

use crate::client::Client;
use crate::paths;
use crate::render;
use crate::term::{Term, Tone};
use crate::util::{now_unix, sanitize};

const MAX_LISTED: usize = 10;

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

pub(crate) fn run(name: &str, sub: &ArgMatches, env: &Env) -> Result<()> {
    let client = Client::new(paths::client_socket(env, paths::is_root())?);
    let json = sub.get_flag("json");
    let term = if json {
        Term::new(false, None)
    } else {
        Term::detect(env)
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("не удалось запустить среду выполнения")?;
    runtime.block_on(dispatch(name, sub, &client, term, json))
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
        "nodes" => nodes(client, term, json, sub.get_flag("all")).await,
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
        out(&render::status(term, &status, now_unix()));
    }
    Ok(())
}

async fn nodes(client: &Client, term: Term, json: bool, all: bool) -> Result<()> {
    let nodes = client.nodes().await?;
    if json {
        out(&to_json(&nodes)?);
    } else {
        out(&render::nodes(term, &nodes, all));
    }
    Ok(())
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
        bail!(
            "не удалось обновить подписок: {failed} из {}",
            updates.results.len()
        );
    }
    Ok(())
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
                    out(&render::event_line(term, now_unix(), &event));
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
        assert!(matches!(by_subscription, ResolveError::Ambiguous { ref ids, .. } if ids.len() == 2));
    }

    #[test]
    fn a_long_list_of_candidates_is_cut() {
        let nodes: Vec<Node> = (0..15).map(|index| node("main", &format!("DE-{index:02}"))).collect();
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
        assert!(matches!(resolve(&nodes, "  ").unwrap_err(), ResolveError::NotFound(_)));
        assert!(matches!(resolve(&[], "x").unwrap_err(), ResolveError::NotFound(_)));
    }

    #[test]
    fn the_query_cannot_inject_terminal_codes_into_the_error() {
        let error = resolve(&sample(), "\x1b[2J").unwrap_err();
        assert!(!error.to_string().contains('\x1b'));
    }
}
