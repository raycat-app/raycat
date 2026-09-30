//! Стенд для CI: печатает, ставит и снимает правила (`install` и `remove` требуют root).
//!
//!   ctl print|install|remove [--kill-switch] [--ipv6] [--bypass СЕТЬ,СЕТЬ]
//!       [--port N] [--own-mark N] [--intercept-mark N] [--table N] [--priority N]
//!   ctl defaults

use std::env;

use anyhow::{Context, Result, anyhow, bail};
use raycat_netfilter::{
    Cidr, DEFAULT_INTERCEPT_MARK, DEFAULT_OWN_MARK, DEFAULT_ROUTE_TABLE, DEFAULT_RULE_PRIORITY,
    DEFAULT_TPROXY_PORT, Rules, install, remove, ruleset,
};

fn number(text: &str) -> Result<u32> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.with_context(|| format!("не число: {text}"))
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(|| anyhow!("нужна команда"))?;
    if command == "defaults" {
        println!("own_mark {DEFAULT_OWN_MARK:#x}");
        println!("intercept_mark {DEFAULT_INTERCEPT_MARK:#x}");
        println!("table {DEFAULT_ROUTE_TABLE}");
        println!("priority {DEFAULT_RULE_PRIORITY}");
        println!("port {DEFAULT_TPROXY_PORT}");
        return Ok(());
    }

    let mut rules = Rules::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow!("у {flag} нет значения"));
        match flag.as_str() {
            "--kill-switch" => rules.kill_switch = true,
            "--ipv6" => rules.intercept_ipv6 = true,
            "--bypass" => {
                rules.bypass = value()?
                    .split(',')
                    .map(|net| {
                        net.parse::<Cidr>()
                            .map_err(|error| anyhow!("{net}: {error}"))
                    })
                    .collect::<Result<_>>()?;
            }
            "--port" => rules.tproxy_port = number(&value()?)?.try_into()?,
            "--own-mark" => rules.own_mark = number(&value()?)?,
            "--intercept-mark" => rules.intercept_mark = number(&value()?)?,
            "--table" => rules.route_table = number(&value()?)?,
            "--priority" => rules.rule_priority = number(&value()?)?,
            other => bail!("неизвестный флаг {other}"),
        }
    }

    match command.as_str() {
        "print" => print!("{}", ruleset(&rules)?),
        "install" => install(&rules)?,
        "remove" => remove(&rules)?,
        other => bail!("неизвестная команда {other}"),
    }
    Ok(())
}
