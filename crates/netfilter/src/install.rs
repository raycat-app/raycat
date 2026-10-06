use anyhow::{Result, bail};

use crate::exec::{Executor, Output, System};
use crate::rules::Rules;
use crate::ruleset::{FAMILY, TABLE, removal, ruleset};

const MAX_RULE_COPIES: usize = 16;

#[derive(Debug, Clone, Copy)]
enum Family {
    V4,
    V6,
}

impl Family {
    fn flag(self) -> &'static str {
        match self {
            Self::V4 => "-4",
            Self::V6 => "-6",
        }
    }
}

/// Ставит перехват: политику маршрутизации, затем таблицу nftables одной транзакцией.
///
/// Маршрутизация идёт первой: пока её нет, помеченный пакет ушёл бы через
/// маршрут по умолчанию мимо xray. Повторный вызов безопасен и не плодит правил.
/// Нужны `CAP_NET_ADMIN` и бинарники `ip` и `nft` в `PATH`.
pub fn install(rules: &Rules) -> Result<()> {
    install_with(&System, rules)
}

/// Снимает всё, что поставил [`install`] с теми же `rules`. Каждый шаг выполняется,
/// даже если предыдущий не удался; отсутствие правил ошибкой не считается.
pub fn remove(rules: &Rules) -> Result<()> {
    remove_with(&System, rules)
}

/// Всё ли, что поставил [`install`], на месте: таблица nftables, правило `ip rule` и
/// локальный маршрут в таблице перехвата. Kill switch опирается на правило
/// маршрутизации, поэтому без него схема не держит. Любая ошибка команд означает «нет».
pub fn is_installed(rules: &Rules) -> bool {
    installed_with(&System, rules)
}

fn installed_with(exec: &dyn Executor, rules: &Rules) -> bool {
    let table = exec
        .run("nft", &["list", "table", FAMILY, TABLE], None)
        .is_ok_and(|output| output.success);
    if !table {
        return false;
    }
    let families: &[Family] = if rules.intercept_ipv6 {
        &[Family::V4, Family::V6]
    } else {
        &[Family::V4]
    };
    families
        .iter()
        .all(|family| routing_present(exec, rules, *family))
}

fn routing_present(exec: &dyn Executor, rules: &Rules, family: Family) -> bool {
    let routing = Routing::new(rules, family);
    let rule = exec
        .run("ip", &[routing.flag, "rule", "show"], None)
        .is_ok_and(|output| output.success && routing.count(&output.stdout) > 0);
    let route = exec
        .run(
            "ip",
            &[routing.flag, "route", "show", "table", &routing.table],
            None,
        )
        .is_ok_and(|output| output.success && output.stdout.contains("local"));
    rule && route
}

fn install_with(exec: &dyn Executor, rules: &Rules) -> Result<()> {
    let text = ruleset(rules)?;
    setup_routing(exec, rules, Family::V4)?;
    if rules.intercept_ipv6 {
        setup_routing(exec, rules, Family::V6)?;
    }
    let output = exec.run("nft", &["-f", "-"], Some(&text))?;
    if !output.success {
        bail!("nft не принял правила: {}", output.stderr);
    }
    Ok(())
}

fn remove_with(exec: &dyn Executor, rules: &Rules) -> Result<()> {
    let mut problems = Vec::new();
    match exec.run("nft", &["-f", "-"], Some(&removal())) {
        Ok(output) if output.success => {}
        Ok(output) => problems.push(format!("nft: {}", output.stderr)),
        Err(error) => problems.push(format!("{error:#}")),
    }
    for family in [Family::V4, Family::V6] {
        if let Err(error) = teardown_routing(exec, rules, family) {
            problems.push(format!("{error:#}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!(
            "не удалось снять правила перехвата: {}",
            problems.join("; ")
        )
    }
}

struct Routing {
    flag: &'static str,
    mark: String,
    table: String,
    priority: String,
}

impl Routing {
    fn new(rules: &Rules, family: Family) -> Self {
        Self {
            flag: family.flag(),
            mark: format!("{:#x}", rules.intercept_mark),
            table: rules.route_table.to_string(),
            priority: rules.rule_priority.to_string(),
        }
    }

    fn rule_args<'a>(&'a self, action: &'a str) -> [&'a str; 9] {
        [
            self.flag,
            "rule",
            action,
            "fwmark",
            &self.mark,
            "lookup",
            &self.table,
            "priority",
            &self.priority,
        ]
    }

    /// Сколько правил `fwmark → таблица` уже есть с нашим приоритетом.
    fn count(&self, listing: &str) -> usize {
        listing.lines().filter(|line| self.is_ours(line)).count()
    }

    fn is_ours(&self, line: &str) -> bool {
        let mut tokens = line.split_whitespace();
        let Some(first) = tokens.next() else {
            return false;
        };
        if first.strip_suffix(':') != Some(self.priority.as_str()) {
            return false;
        }
        let (mut mark, mut table) = (false, false);
        while let Some(token) = tokens.next() {
            match token {
                "fwmark" => {
                    mark = tokens
                        .next()
                        .is_some_and(|value| value.split('/').next() == Some(self.mark.as_str()));
                }
                "lookup" | "table" => {
                    table = tokens.next() == Some(self.table.as_str());
                }
                _ => {}
            }
        }
        mark && table
    }
}

fn ip(exec: &dyn Executor, args: &[&str]) -> Result<Output> {
    let output = exec.run("ip", args, None)?;
    if !output.success {
        bail!(
            "команда `ip {}` не выполнена: {}",
            args.join(" "),
            output.stderr
        );
    }
    Ok(output)
}

fn setup_routing(exec: &dyn Executor, rules: &Rules, family: Family) -> Result<()> {
    let routing = Routing::new(rules, family);
    ip(
        exec,
        &[
            routing.flag,
            "route",
            "replace",
            "local",
            "default",
            "dev",
            "lo",
            "table",
            &routing.table,
        ],
    )?;
    let listing = ip(exec, &[routing.flag, "rule", "show"])?;
    match routing.count(&listing.stdout) {
        0 => {
            ip(exec, &routing.rule_args("add"))?;
        }
        1 => {}
        copies => {
            for _ in 1..copies {
                ip(exec, &routing.rule_args("del"))?;
            }
        }
    }
    Ok(())
}

/// Ошибки `ip` здесь игнорируются: команды падают, когда снимать уже нечего, а
/// для IPv6 — и когда его нет в ядре.
fn teardown_routing(exec: &dyn Executor, rules: &Rules, family: Family) -> Result<()> {
    let routing = Routing::new(rules, family);
    for _ in 0..MAX_RULE_COPIES {
        if !exec.run("ip", &routing.rule_args("del"), None)?.success {
            break;
        }
    }
    exec.run(
        "ip",
        &[routing.flag, "route", "flush", "table", &routing.table],
        None,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    type Handler = Box<dyn Fn(&str) -> Output>;

    struct Fake {
        calls: RefCell<Vec<String>>,
        inputs: RefCell<Vec<String>>,
        handler: Handler,
    }

    impl Fake {
        fn new(handler: impl Fn(&str) -> Output + 'static) -> Self {
            Self {
                calls: RefCell::default(),
                inputs: RefCell::default(),
                handler: Box::new(handler),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Executor for Fake {
        fn run(&self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<Output> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            if let Some(stdin) = stdin {
                self.inputs.borrow_mut().push(stdin.to_owned());
            }
            Ok((self.handler)(&line))
        }
    }

    fn ok(stdout: &str) -> Output {
        Output {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn failed(stderr: &str) -> Output {
        Output {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }
    }

    const DEFAULT_RULES: &str = "0:\tfrom all lookup local\n32766:\tfrom all lookup main\n32767:\tfrom all lookup default\n";
    const OURS: &str = "7263:\tfrom all fwmark 0x52540000 lookup 7263\n";

    fn listing(ours: usize) -> String {
        format!(
            "0:\tfrom all lookup local\n{}32766:\tfrom all lookup main\n",
            OURS.repeat(ours)
        )
    }

    #[test]
    fn install_sets_routing_then_loads_the_ruleset() {
        let fake = Fake::new(|line| {
            if line == "ip -4 rule show" {
                ok(DEFAULT_RULES)
            } else {
                ok("")
            }
        });
        let rules = Rules::default();
        install_with(&fake, &rules).unwrap();
        assert_eq!(
            fake.calls(),
            [
                "ip -4 route replace local default dev lo table 7263",
                "ip -4 rule show",
                "ip -4 rule add fwmark 0x52540000 lookup 7263 priority 7263",
                "nft -f -",
            ]
        );
        assert_eq!(*fake.inputs.borrow(), [ruleset(&rules).unwrap()]);
    }

    #[test]
    fn install_is_idempotent_and_collapses_duplicates() {
        let once = Fake::new(|line| {
            if line == "ip -4 rule show" {
                ok(&listing(1))
            } else {
                ok("")
            }
        });
        install_with(&once, &Rules::default()).unwrap();
        assert!(
            !once
                .calls()
                .iter()
                .any(|call| call.contains("rule add") || call.contains("rule del"))
        );

        let thrice = Fake::new(|line| {
            if line == "ip -4 rule show" {
                ok(&listing(3))
            } else {
                ok("")
            }
        });
        install_with(&thrice, &Rules::default()).unwrap();
        let dels = thrice
            .calls()
            .iter()
            .filter(|call| call.contains("rule del"))
            .count();
        assert_eq!(dels, 2);
        assert!(!thrice.calls().iter().any(|call| call.contains("rule add")));
    }

    #[test]
    fn foreign_rules_are_not_counted() {
        let rules = Rules::default();
        let routing = Routing::new(&rules, Family::V4);
        let foreign = "7263:\tfrom all fwmark 0x1 lookup 7263\n\
                       100:\tfrom all fwmark 0x52540000 lookup 7263\n\
                       7263:\tfrom all fwmark 0x52540000 lookup 254\n\
                       7263:\tfrom all lookup 7263\n";
        assert_eq!(routing.count(foreign), 0);
        assert_eq!(
            routing.count("7263:\tfrom all fwmark 0x52540000/0xffffffff lookup 7263\n"),
            1
        );
    }

    #[test]
    fn ipv6_routing_is_set_only_when_intercepted() {
        let fake = Fake::new(|_| ok(""));
        install_with(&fake, &Rules::default()).unwrap();
        assert!(!fake.calls().iter().any(|call| call.contains("-6")));

        let fake = Fake::new(|_| ok(""));
        let rules = Rules {
            intercept_ipv6: true,
            ..Rules::default()
        };
        install_with(&fake, &rules).unwrap();
        let calls = fake.calls();
        assert!(calls.contains(&"ip -6 route replace local default dev lo table 7263".to_owned()));
        assert!(
            calls
                .contains(&"ip -6 rule add fwmark 0x52540000 lookup 7263 priority 7263".to_owned())
        );
        assert_eq!(calls.last().unwrap(), "nft -f -");
    }

    #[test]
    fn custom_values_reach_the_commands() {
        let fake = Fake::new(|_| ok(""));
        let rules = Rules {
            intercept_mark: 0x1000_0002,
            route_table: 4711,
            rule_priority: 99,
            ..Rules::default()
        };
        install_with(&fake, &rules).unwrap();
        assert!(
            fake.calls()
                .contains(&"ip -4 rule add fwmark 0x10000002 lookup 4711 priority 99".to_owned())
        );
    }

    #[test]
    fn failures_carry_the_command_output() {
        let fake = Fake::new(|line| {
            if line.starts_with("nft") {
                failed("Error: unsupported chain type")
            } else {
                ok("")
            }
        });
        let error = install_with(&fake, &Rules::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("nft"), "{error}");
        assert!(error.contains("unsupported chain type"), "{error}");

        let fake = Fake::new(|line| {
            if line.contains("route replace") {
                failed("RTNETLINK answers: Operation not permitted")
            } else {
                ok("")
            }
        });
        let error = install_with(&fake, &Rules::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("ip -4 route replace"), "{error}");
        assert!(error.contains("Operation not permitted"), "{error}");
        assert!(
            !fake.calls().iter().any(|call| call.starts_with("nft")),
            "правила без маршрутизации не ставятся"
        );
    }

    #[test]
    fn invalid_rules_run_nothing() {
        let fake = Fake::new(|_| ok(""));
        let rules = Rules {
            tproxy_port: 0,
            ..Rules::default()
        };
        assert!(install_with(&fake, &rules).is_err());
        assert_eq!(fake.calls(), Vec::<String>::new());
    }

    #[test]
    fn remove_deletes_every_copy_and_tolerates_missing_things() {
        let left = Cell::new(2);
        let fake = Fake::new(move |line| {
            if line.starts_with("ip -4 rule del") && left.get() > 0 {
                left.set(left.get() - 1);
                ok("")
            } else if line.starts_with("ip") {
                failed("RTNETLINK answers: No such file or directory")
            } else {
                ok("")
            }
        });
        remove_with(&fake, &Rules::default()).unwrap();
        let calls = fake.calls();
        assert_eq!(calls[0], "nft -f -");
        assert_eq!(*fake.inputs.borrow(), [removal()]);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("ip -4 rule del"))
                .count(),
            3
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("ip -6 rule del"))
                .count(),
            1
        );
        assert!(calls.contains(&"ip -4 route flush table 7263".to_owned()));
        assert!(calls.contains(&"ip -6 route flush table 7263".to_owned()));
    }

    fn healthy(line: &str) -> Output {
        match line {
            "ip -4 rule show" | "ip -6 rule show" => ok(&listing(1)),
            "ip -4 route show table 7263" | "ip -6 route show table 7263" => {
                ok("local default dev lo scope host\n")
            }
            _ => ok(""),
        }
    }

    #[test]
    fn a_complete_installation_is_recognised() {
        let fake = Fake::new(healthy);
        assert!(installed_with(&fake, &Rules::default()));
        assert_eq!(fake.calls()[0], "nft list table inet raycat");
    }

    #[test]
    fn a_missing_table_rule_or_route_is_noticed() {
        let no_table = Fake::new(|line| {
            if line.starts_with("nft") {
                failed("Error: No such file or directory")
            } else {
                healthy(line)
            }
        });
        assert!(!installed_with(&no_table, &Rules::default()));

        let no_rule = Fake::new(|line| {
            if line == "ip -4 rule show" {
                ok(DEFAULT_RULES)
            } else {
                healthy(line)
            }
        });
        assert!(!installed_with(&no_rule, &Rules::default()));

        let no_route = Fake::new(|line| {
            if line.contains("route show") {
                ok("")
            } else {
                healthy(line)
            }
        });
        assert!(!installed_with(&no_route, &Rules::default()));
    }

    #[test]
    fn ipv6_routing_is_checked_only_when_intercepted() {
        let broken_v6 = |line: &str| {
            if line.starts_with("ip -6") {
                ok("")
            } else {
                healthy(line)
            }
        };
        assert!(installed_with(&Fake::new(broken_v6), &Rules::default()));
        let rules = Rules {
            intercept_ipv6: true,
            ..Rules::default()
        };
        assert!(!installed_with(&Fake::new(broken_v6), &rules));
        assert!(installed_with(&Fake::new(healthy), &rules));
    }

    #[test]
    fn remove_finishes_the_job_when_nft_fails() {
        let fake = Fake::new(|line| {
            if line.starts_with("nft") {
                failed("Error: Operation not permitted")
            } else {
                ok("")
            }
        });
        let error = remove_with(&fake, &Rules::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("Operation not permitted"), "{error}");
        assert!(fake.calls().iter().any(|call| call.contains("route flush")));
    }
}
