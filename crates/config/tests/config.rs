// Хелперы тестов не покрыты allow-unwrap-in-tests: там ошибка и есть падение теста.
#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use raycat_config::{
    App, Config, DEFAULT_UPDATE_INTERVAL, Env, Error, Lan, LogLevel, Mode, Platform, TcpCongestion,
};

const FULL: &str = r#"
[device]
seed = "любая фраза"
hostname = "DESKTOP-TEST01"
model = "SM-S921B"
manufacturer = "samsung"
os_version = "14"
locale = "ru"

[[subscription]]
name = "основная"
url = "https://sub.example.com/api/sub/AbCdEfGh1234?x=1"
app = "happ"
platform = "windows"
update_interval = "6h"
allow = ["*Германия*"]
deny = ["*Россия*", "*Info*"]
priority = ["*NL*"]

[[subscription]]
name = "резерв"
url = "http://203.0.113.7:8080/sub/reserve-token-9876"
allow_http = true
app = "incy"
platform = "android"
seed = "другое устройство"

[selection]
check_url = "https://www.gstatic.com/generate_204"
check_interval = "45s"
failures = 5
switch_gain = "200ms"
return_delay = "10m"
pin = "основная/🇳🇱 Нидерланды 1/A"

[mode]
type = "gateway"
kill_switch = false
lan = true
lan_interface = "br-lan.10"
lan_subnets = ["192.168.1.0/24", "10.8.0.0/16"]

[dns]
resolvers = ["9.9.9.9", "2606:4700:4700::1111"]

[routing]
provider = true

[xray]
path = "/opt/xray/xray"
memory_limit = "64MiB"
tcp_congestion = "Cubic"
xhttp_connections = 4

[log]
level = "debug"
"#;

const OK_SUB: &str = r#"
[[subscription]]
name = "основная"
url = "https://sub.example.com/api/sub/AbCdEfGh1234"
app = "happ"
platform = "windows"
"#;

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn parse(text: &str) -> Config {
    Config::from_toml_str(text, &Env::new()).unwrap()
}

fn problems_with_env(text: &str, env: &Env) -> Vec<String> {
    match Config::from_toml_str(text, env) {
        Err(Error::Invalid(list)) => list.iter().map(ToString::to_string).collect(),
        other => panic!("ожидались ошибки проверки, получено {other:?}"),
    }
}

fn problems(text: &str) -> Vec<String> {
    problems_with_env(text, &Env::new())
}

fn has(list: &[String], field: &str) -> bool {
    list.iter()
        .any(|line| line.starts_with(&format!("{field}: ")))
}

fn temp_file(name: &str, content: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "raycat-config-test-{}-{name}.toml",
        std::process::id()
    ));
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn full_example() {
    let config = parse(FULL);

    assert_eq!(config.device.seed.as_ref().unwrap().expose(), "любая фраза");
    assert!(config.device.machine_id.is_none());
    assert_eq!(config.device.hostname.as_deref(), Some("DESKTOP-TEST01"));
    assert_eq!(config.device.model.as_deref(), Some("SM-S921B"));
    assert_eq!(config.device.manufacturer.as_deref(), Some("samsung"));
    assert_eq!(config.device.os_version.as_deref(), Some("14"));
    assert_eq!(config.device.locale.as_deref(), Some("ru"));

    assert_eq!(config.subscriptions.len(), 2);
    let main = &config.subscriptions[0];
    assert_eq!(main.name, "основная");
    assert_eq!(
        main.url.expose(),
        "https://sub.example.com/api/sub/AbCdEfGh1234?x=1"
    );
    assert_eq!((main.app, main.platform), (App::Happ, Platform::Windows));
    assert!(main.seed.is_none());
    assert_eq!(main.update_interval, Some(Duration::from_secs(6 * 3_600)));
    assert_eq!(main.allow.len(), 1);
    assert_eq!(main.deny.len(), 2);
    assert_eq!(main.priority[0].as_str(), "*NL*");
    assert_eq!(main.masked_url(), "https://sub.example.com/…1234");

    let reserve = &config.subscriptions[1];
    assert_eq!(reserve.name, "резерв");
    assert_eq!(
        (reserve.app, reserve.platform),
        (App::Incy, Platform::Android)
    );
    assert_eq!(reserve.seed.as_ref().unwrap().expose(), "другое устройство");
    assert_eq!(reserve.update_interval, None);
    assert!(reserve.allow.is_empty() && reserve.deny.is_empty() && reserve.priority.is_empty());

    let selection = &config.selection;
    assert_eq!(selection.check_url, "https://www.gstatic.com/generate_204");
    assert_eq!(selection.check_interval, Duration::from_secs(45));
    assert_eq!(selection.failures, 5);
    assert_eq!(selection.switch_gain, Duration::from_millis(200));
    assert_eq!(selection.return_delay, Duration::from_secs(600));
    let pin = selection.pin.as_ref().unwrap();
    assert_eq!(pin.subscription, "основная");
    assert_eq!(pin.node, "🇳🇱 Нидерланды 1/A");

    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: false,
            lan: true
        }
    );
    assert_eq!(config.lan.interface.as_deref(), Some("br-lan.10"));
    let subnets: Vec<String> = config.lan.subnets.iter().map(ToString::to_string).collect();
    assert_eq!(subnets, ["192.168.1.0/24", "10.8.0.0/16"]);
    assert_eq!(
        config.dns.resolvers,
        [
            IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)),
            IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
        ]
    );
    assert!(config.routing.provider);
    assert_eq!(config.xray.path, PathBuf::from("/opt/xray/xray"));
    assert_eq!(config.xray.memory_limit, 64 << 20);
    assert_eq!(
        config.xray.tcp_congestion,
        TcpCongestion::Algorithm("cubic".to_owned())
    );
    assert_eq!(config.xray.xhttp_connections, Some(4));
    assert_eq!(config.log.level, LogLevel::Debug);
}

#[test]
fn defaults() {
    let config = parse(OK_SUB);

    assert!(config.device.seed.is_none() && config.device.machine_id.is_none());
    assert!(config.device.hostname.is_none() && config.device.model.is_none());
    assert!(config.device.manufacturer.is_none());
    assert!(config.device.os_version.is_none() && config.device.locale.is_none());

    let sub = &config.subscriptions[0];
    assert_eq!(sub.update_interval, None);
    assert_eq!(DEFAULT_UPDATE_INTERVAL, Duration::from_secs(12 * 3_600));
    assert!(sub.allow.is_empty() && sub.deny.is_empty() && sub.priority.is_empty());

    let selection = &config.selection;
    assert_eq!(selection.check_url, "https://www.gstatic.com/generate_204");
    assert_eq!(selection.check_interval, Duration::from_secs(30));
    assert_eq!(selection.failures, 3);
    assert_eq!(selection.switch_gain, Duration::from_millis(150));
    assert_eq!(selection.return_delay, Duration::from_secs(300));
    assert!(selection.pin.is_none());

    assert_eq!(
        config.mode,
        Mode::Proxy {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7890)
        }
    );
    assert_eq!(
        config.dns.resolvers,
        [
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
        ]
    );
    assert!(!config.routing.provider);
    assert_eq!(config.xray.path, PathBuf::from("/usr/libexec/raycat/xray"));
    assert_eq!(config.xray.memory_limit, 96 << 20);
    assert_eq!(config.xray.tcp_congestion, TcpCongestion::Auto);
    assert_eq!(config.xray.xhttp_connections, None);
    assert_eq!(config.log.level, LogLevel::Info);
}

#[test]
fn tcp_congestion_values() {
    let cases = [
        ("auto", TcpCongestion::Auto),
        ("OFF", TcpCongestion::Off),
        (" Auto ", TcpCongestion::Auto),
        ("bbr", TcpCongestion::Algorithm("bbr".to_owned())),
        ("BBR", TcpCongestion::Algorithm("bbr".to_owned())),
        ("my_cc-2", TcpCongestion::Algorithm("my_cc-2".to_owned())),
        (
            "abcdefghijklmno",
            TcpCongestion::Algorithm("abcdefghijklmno".to_owned()),
        ),
    ];
    for (value, expected) in cases {
        let config = parse(&format!("{OK_SUB}\n[xray]\ntcp_congestion = \"{value}\"\n"));
        assert_eq!(config.xray.tcp_congestion, expected, "{value}");
    }
}

#[test]
fn xhttp_connections_bounds_are_inclusive() {
    for (value, expected) in [(1, 1), (8, 8), (16, 16)] {
        let config = parse(&format!("{OK_SUB}\n[xray]\nxhttp_connections = {value}\n"));
        assert_eq!(config.xray.xhttp_connections, Some(expected));
    }
}

#[test]
fn performance_option_errors_are_in_russian_and_name_the_field() {
    let cases = [
        ("tcp_congestion = \"\"", "xray.tcp_congestion"),
        ("tcp_congestion = \"bbr!\"", "xray.tcp_congestion"),
        (
            "tcp_congestion = \"abcdefghijklmnop\"",
            "xray.tcp_congestion",
        ),
        ("tcp_congestion = \"bbr cubic\"", "xray.tcp_congestion"),
        ("xhttp_connections = 0", "xray.xhttp_connections"),
        ("xhttp_connections = 17", "xray.xhttp_connections"),
        ("xhttp_connections = -1", "xray.xhttp_connections"),
        ("xhttp_connections = 300", "xray.xhttp_connections"),
    ];
    for (line, field) in cases {
        let list = problems(&format!("{OK_SUB}\n[xray]\n{line}\n"));
        assert_eq!(list.len(), 1, "{line}: {list:?}");
        assert!(has(&list, field), "{line}: {list:?}");
    }
    let list = problems(&format!("{OK_SUB}\n[xray]\nxhttp_connections = 0\n"));
    assert!(list[0].contains("от 1 до 16"), "{list:?}");
    let list = problems(&format!("{OK_SUB}\n[xray]\ntcp_congestion = \"x!\"\n"));
    assert!(list[0].contains("допустимо"), "{list:?}");
}

#[test]
fn xhttp_connections_must_be_a_number() {
    assert!(
        Config::from_toml_str(
            &format!("{OK_SUB}\n[xray]\nxhttp_connections = \"4\"\n"),
            &Env::new()
        )
        .is_err()
    );
}

#[test]
fn gateway_defaults_keep_the_kill_switch_on() {
    let config = parse(&format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\n"));
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: false
        }
    );
}

#[test]
fn lan_details_are_left_to_the_daemon_by_default() {
    let config = parse(&format!(
        "{OK_SUB}\n[mode]\ntype = \"gateway\"\nlan = true\n"
    ));
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: true
        }
    );
    assert_eq!(config.lan.interface, None);
    assert_eq!(config.lan.subnets, Vec::<raycat_netfilter::Cidr>::new());
}

#[test]
fn lan_details_are_checked() {
    let gateway =
        |extra: &str| format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\nlan = true\n{extra}\n");
    let list = problems(&gateway(
        "lan_interface = \"eth0; drop\"\nlan_subnets = [\"fd00::/64\", \"10.0.0.5/32\", \"mars\", \"10.1.2.3/24\", \"192.168.1.0/24\"]",
    ));
    assert_eq!(list.len(), 5, "{list:?}");
    assert!(has(&list, "mode.lan_interface"), "{list:?}");
    for index in 0..4 {
        assert!(
            has(&list, &format!("mode.lan_subnets[{index}]")),
            "{index}: {list:?}"
        );
    }
    assert!(list.iter().any(|line| line.contains("только IPv4")));
    assert!(list.iter().any(|line| line.contains("от 8 до 31")));
    assert!(list.iter().any(|line| line.contains("биты хоста")));

    let list = problems(&gateway("lan_subnets = []"));
    assert!(has(&list, "mode.lan_subnets"), "{list:?}");
    let many = format!("lan_subnets = [{}]", "\"10.0.0.0/24\",".repeat(33));
    let list = problems(&gateway(&many));
    assert!(has(&list, "mode.lan_subnets"), "{list:?}");
}

#[test]
fn lan_details_do_not_turn_the_lan_gateway_on() {
    let text = format!(
        "{OK_SUB}\n[mode]\ntype = \"gateway\"\nlan_interface = \"eth0\"\nlan_subnets = [\"10.0.0.0/24\"]\n"
    );
    let config = parse(&text);
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: false
        }
    );
    assert_eq!(config.lan, Lan::default());
    assert_eq!(
        config.warnings,
        [
            "mode.lan_interface действует только при mode.lan = true",
            "mode.lan_subnets действует только при mode.lan = true",
        ]
    );
}

#[test]
fn lan_details_are_not_checked_without_the_lan_gateway() {
    let text = format!(
        "{OK_SUB}\n[mode]\ntype = \"gateway\"\nlan_interface = \"eth0; drop\"\nlan_subnets = [\"mars\"]\n"
    );
    let config = parse(&text);
    assert_eq!(config.lan, Lan::default());
    assert_eq!(config.warnings.len(), 2, "{:?}", config.warnings);
    let proxy = format!("{OK_SUB}\n[mode]\nlan_subnets = [\"mars\"]\n");
    assert_eq!(parse(&proxy).lan, Lan::default());
}

#[test]
fn proxy_warns_about_gateway_keys() {
    let text = format!("{OK_SUB}\n[mode]\nkill_switch = false\nlan = true\n");
    let config = parse(&text);
    assert!(matches!(config.mode, Mode::Proxy { .. }));
    assert_eq!(
        config.warnings,
        [
            "mode.kill_switch действует только в режиме gateway — в режиме proxy он не применяется",
            "mode.lan действует только в режиме gateway — в режиме proxy он не применяется",
        ]
    );
}

#[test]
fn gateway_warns_about_listen_and_ignores_its_value() {
    let text = format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\nlisten = \"nope\"\n");
    let config = parse(&text);
    assert_eq!(
        config.warnings,
        ["mode.listen действует только в режиме proxy"]
    );
    let config = Config::from_toml_str(
        &format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\n"),
        &env(&[("RAYCAT_LISTEN", "nowhere")]),
    )
    .unwrap();
    assert_eq!(
        config.warnings,
        ["RAYCAT_LISTEN действует только в режиме proxy"]
    );
}

#[test]
fn environment_names_the_variable_in_warnings() {
    let vars = env(&[("RAYCAT_KILL_SWITCH", "true"), ("RAYCAT_LAN", "1")]);
    let config = Config::from_toml_str(&format!("{OK_SUB}\n[mode]\n"), &vars).unwrap();
    assert_eq!(
        config.warnings,
        [
            "RAYCAT_KILL_SWITCH действует только в режиме gateway — в режиме proxy он не применяется",
            "RAYCAT_LAN действует только в режиме gateway — в режиме proxy он не применяется",
        ]
    );
}

#[test]
fn listen_is_checked_only_for_the_proxy() {
    let text = format!("{OK_SUB}\n[mode]\nlisten = \"nope\"\n");
    let list = problems(&text);
    assert!(has(&list, "mode.listen"), "{list:?}");
    let vars = env(&[("RAYCAT_LISTEN", "nowhere")]);
    let list = problems_with_env(OK_SUB, &vars);
    assert!(has(&list, "RAYCAT_LISTEN"), "{list:?}");
}

#[test]
fn applicable_keys_give_no_warnings() {
    let none = Vec::<String>::new();
    assert_eq!(parse(FULL).warnings, none);
    assert_eq!(parse(OK_SUB).warnings, none);
    let text = format!("{OK_SUB}\n[mode]\nlisten = \"127.0.0.1:1080\"\n");
    assert_eq!(parse(&text).warnings, none);
}

#[test]
fn environment_turns_the_lan_gateway_on() {
    let file = format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\n");
    let config = Config::from_toml_str(&file, &env(&[("RAYCAT_LAN", "yes")])).unwrap();
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: true
        }
    );
    let off = format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\nlan = true\n");
    let config = Config::from_toml_str(&off, &env(&[("RAYCAT_LAN", "0")])).unwrap();
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: false
        }
    );
    let config = Config::from_toml_str(&off, &env(&[("RAYCAT_LAN", "")])).unwrap();
    assert!(matches!(config.mode, Mode::Gateway { lan: true, .. }));
    let list = problems_with_env(&file, &env(&[("RAYCAT_LAN", "maybe")]));
    assert!(has(&list, "RAYCAT_LAN"), "{list:?}");
}

#[test]
fn proxy_listen_address() {
    let config = parse(&format!("{OK_SUB}\n[mode]\nlisten = \"[::1]:1080\"\n"));
    assert_eq!(
        config.mode,
        Mode::Proxy {
            listen: "[::1]:1080".parse().unwrap()
        }
    );
}

#[test]
fn machine_id_is_lowercased() {
    let config = parse(&format!(
        "[device]\nmachine_id = \"0123456789ABCDEF0123456789abcdef\"\n{OK_SUB}"
    ));
    let id = config.device.machine_id.unwrap();
    assert_eq!(id.expose(), "0123456789abcdef0123456789abcdef");
}

#[test]
fn enum_values_ignore_case() {
    let config = parse(
        r#"
[[subscription]]
name = "a"
url = "https://sub.example.com/x/abcd"
app = "Happ"
platform = "ANDROID"

[log]
level = "WARN"
"#,
    );
    assert_eq!(config.subscriptions[0].app, App::Happ);
    assert_eq!(config.subscriptions[0].platform, Platform::Android);
    assert_eq!(config.log.level, LogLevel::Warn);
}

#[test]
fn node_filters() {
    let config = parse(FULL);
    let main = &config.subscriptions[0];
    assert!(main.allows("🇩🇪 Германия 1"));
    assert!(!main.allows("🇫🇮 Финляндия"));
    assert!(!main.allows("🇩🇪 Германия Info"));
    assert!(!main.allows("Россия Германия"));
    assert!(config.subscriptions[1].allows("что угодно"));
}

#[test]
fn no_subscriptions() {
    let list = problems("");
    assert_eq!(list.len(), 1);
    assert!(has(&list, "subscription"), "{list:?}");
}

#[test]
fn subscription_field_errors() {
    let cases = [
        (
            "update_interval = \"5m\"",
            "subscription[0].update_interval",
        ),
        (
            "update_interval = \"31d\"",
            "subscription[0].update_interval",
        ),
        (
            "update_interval = \"soon\"",
            "subscription[0].update_interval",
        ),
        (
            "update_interval = \"12\"",
            "subscription[0].update_interval",
        ),
        ("allow = [\"\"]", "subscription[0].allow[0]"),
        ("deny = [\"ok\", \"  \"]", "subscription[0].deny[1]"),
        ("priority = [\"\"]", "subscription[0].priority[0]"),
        ("seed = \"\"", "subscription[0].seed"),
        ("seed = \"   \"", "subscription[0].seed"),
    ];
    for (extra, field) in cases {
        let list = problems(&format!("{OK_SUB}{extra}\n"));
        assert_eq!(list.len(), 1, "{extra}: {list:?}");
        assert!(has(&list, field), "{extra}: {list:?}");
    }
}

#[test]
fn update_interval_bounds_are_inclusive() {
    for value in ["10m", "30d"] {
        parse(&format!("{OK_SUB}update_interval = \"{value}\"\n"));
    }
}

#[test]
fn selection_errors() {
    let cases = [
        ("check_url = \"ftp://example.com/\"", "selection.check_url"),
        ("check_url = \"\"", "selection.check_url"),
        ("check_interval = \"4s\"", "selection.check_interval"),
        ("check_interval = \"11m\"", "selection.check_interval"),
        ("check_interval = \"30\"", "selection.check_interval"),
        ("failures = 0", "selection.failures"),
        ("failures = 21", "selection.failures"),
        ("failures = -1", "selection.failures"),
        ("switch_gain = \"61s\"", "selection.switch_gain"),
        ("return_delay = \"25h\"", "selection.return_delay"),
        ("pin = \"нет-такой/узел\"", "selection.pin"),
        ("pin = \"без-слеша\"", "selection.pin"),
        ("pin = \"основная/\"", "selection.pin"),
    ];
    for (extra, field) in cases {
        let list = problems(&format!("{OK_SUB}\n[selection]\n{extra}\n"));
        assert_eq!(list.len(), 1, "{extra}: {list:?}");
        assert!(has(&list, field), "{extra}: {list:?}");
    }
}

#[test]
fn selection_bounds_are_inclusive() {
    let config = parse(&format!(
        "{OK_SUB}\n[selection]\ncheck_interval = \"5s\"\nfailures = 1\nswitch_gain = \"0ms\"\nreturn_delay = \"0s\"\n"
    ));
    assert_eq!(config.selection.check_interval, Duration::from_secs(5));
    assert_eq!(config.selection.failures, 1);
    assert_eq!(config.selection.switch_gain, Duration::ZERO);
    assert_eq!(config.selection.return_delay, Duration::ZERO);
    parse(&format!(
        "{OK_SUB}\n[selection]\ncheck_interval = \"10m\"\nfailures = 20\n"
    ));
}

#[test]
fn other_section_errors() {
    let cases = [
        ("[mode]\ntype = \"vpn\"", "mode.type"),
        ("[mode]\nlisten = \"localhost:7890\"", "mode.listen"),
        ("[mode]\nlisten = \"127.0.0.1:0\"", "mode.listen"),
        ("[mode]\nlisten = \"7890\"", "mode.listen"),
        ("[dns]\nresolvers = []", "dns.resolvers"),
        (
            "[dns]\nresolvers = [\"dns.example.com\"]",
            "dns.resolvers[0]",
        ),
        (
            "[dns]\nresolvers = [\"1.1.1.1\", \"1.1.1.2\", \"1.1.1.3\", \"1.1.1.4\", \"1.1.1.5\", \"1.1.1.6\", \"1.1.1.7\", \"1.1.1.8\", \"1.1.1.9\"]",
            "dns.resolvers",
        ),
        ("[xray]\npath = \"\"", "xray.path"),
        ("[xray]\nmemory_limit = \"8MiB\"", "xray.memory_limit"),
        ("[xray]\nmemory_limit = \"17GiB\"", "xray.memory_limit"),
        ("[xray]\nmemory_limit = \"lots\"", "xray.memory_limit"),
        ("[log]\nlevel = \"trace\"", "log.level"),
    ];
    for (section, field) in cases {
        let list = problems(&format!("{OK_SUB}\n{section}\n"));
        assert_eq!(list.len(), 1, "{section}: {list:?}");
        assert!(has(&list, field), "{section}: {list:?}");
    }
}

#[test]
fn device_errors() {
    let cases = [
        (
            "seed = \"a\"\nmachine_id = \"00000000000000000000000000000000\"",
            "device",
        ),
        ("seed = \"  \"", "device.seed"),
        ("machine_id = \"abc\"", "device.machine_id"),
        (
            "machine_id = \"0000000000000000000000000000000\"",
            "device.machine_id",
        ),
        (
            "machine_id = \"0000000000000000000000000000000g\"",
            "device.machine_id",
        ),
        ("hostname = \"\"", "device.hostname"),
        ("hostname = \"a\\r\\nX-Injected: 1\"", "device.hostname"),
        ("model = \"a\\tb\"", "device.model"),
        ("manufacturer = \"\"", "device.manufacturer"),
        ("manufacturer = \"a\\r\\nb\"", "device.manufacturer"),
        ("os_version = \"14\\n\"", "device.os_version"),
        ("locale = \"ru\\u0007\"", "device.locale"),
    ];
    for (lines, field) in cases {
        let list = problems(&format!("[device]\n{lines}\n{OK_SUB}"));
        assert_eq!(list.len(), 1, "{lines}: {list:?}");
        assert!(has(&list, field), "{lines}: {list:?}");
    }
}

#[test]
fn subscription_name_errors() {
    let block = |name: &str| {
        format!(
            "[[subscription]]\nname = \"{name}\"\nurl = \"https://sub.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n"
        )
    };
    let list = problems(&format!("{}{}", block("одна"), block("одна")));
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(has(&list, "subscription[1].name"), "{list:?}");
    assert!(list[0].contains("уже используется"), "{list:?}");

    let too_long = "я".repeat(65);
    for name in ["", "   ", "a/b", too_long.as_str()] {
        let list = problems(&block(name));
        assert_eq!(list.len(), 1, "{name:?}: {list:?}");
        assert!(has(&list, "subscription[0].name"), "{name:?}: {list:?}");
    }
    parse(&block(&"я".repeat(64)));
}

#[test]
fn missing_required_subscription_fields() {
    let list = problems("[[subscription]]\nname = \"a\"\n");
    assert_eq!(list.len(), 3, "{list:?}");
    for field in [
        "subscription[0]",
        "subscription[0].app",
        "subscription[0].platform",
    ] {
        assert!(has(&list, field), "{list:?}");
    }
    let list = problems(
        "[[subscription]]\nurl = \"https://sub.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n",
    );
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(has(&list, "subscription[0].name"), "{list:?}");
}

#[test]
fn app_and_platform_must_match() {
    let list = problems(
        r#"
[[subscription]]
name = "a"
url = "https://sub.example.com/x/abcd"
app = "incy"
platform = "windows"
"#,
    );
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(has(&list, "subscription[0].platform"), "{list:?}");
    assert!(list[0].contains("android"), "{list:?}");

    let list = problems(
        r#"
[[subscription]]
name = "a"
url = "https://sub.example.com/x/abcd"
app = "firefox"
platform = "linux"
"#,
    );
    assert_eq!(list.len(), 2, "{list:?}");
    assert!(has(&list, "subscription[0].app") && has(&list, "subscription[0].platform"));
}

#[test]
fn http_needs_an_explicit_permission() {
    let list = problems(
        r#"
[[subscription]]
name = "a"
url = "http://203.0.113.7/sub/AbCdEfGh1234"
app = "happ"
platform = "windows"
"#,
    );
    assert_eq!(list.len(), 1);
    assert!(has(&list, "subscription[0].url"));
    assert!(list[0].contains("allow_http"));
    assert!(list[0].contains("http://203.0.113.7/…1234"));
    assert!(!list[0].contains("AbCdEfGh1234"), "ссылка не замаскирована");
}

#[test]
fn bad_links_are_masked_in_errors() {
    for (i, url) in [
        "https://sub.example.com/a b/SECRETTOKEN9999",
        "ftp://sub.example.com/x/SECRETTOKEN9999",
        "https://user:pass@sub.example.com/x/SECRETTOKEN9999",
        "https://sub.example.com:0/x/SECRETTOKEN9999",
        "SECRETTOKEN9999",
    ]
    .into_iter()
    .enumerate()
    {
        let list = problems(&format!(
            "[[subscription]]\nname = \"a\"\nurl = \"{url}\"\napp = \"happ\"\nplatform = \"windows\"\n"
        ));
        assert_eq!(list.len(), 1, "ссылка №{i}");
        assert!(has(&list, "subscription[0].url"), "ссылка №{i}");
        assert!(list[0].contains("…9999"), "ссылка №{i}");
        assert!(
            !list[0].contains("SECRETTOKEN"),
            "ссылка №{i} не замаскирована"
        );
        assert!(!list[0].contains("pass"), "ссылка №{i}: виден пароль");
    }
}

#[test]
fn all_problems_come_at_once() {
    let list = problems(
        r#"
[device]
machine_id = "zz"
hostname = "a\nb"

[[subscription]]
name = "a"
url = "http://203.0.113.7/x"
app = "incy"
platform = "windows"

[[subscription]]
name = "a"
url = "https://sub.example.com/x/abcd"
app = "happ"
platform = "windows"
allow = [""]

[selection]
failures = 99
pin = "нет/узел"

[mode]
listen = "nope"

[log]
level = "loud"
"#,
    );
    for field in [
        "device.machine_id",
        "device.hostname",
        "subscription[0].url",
        "subscription[0].platform",
        "subscription[1].name",
        "subscription[1].allow[0]",
        "selection.failures",
        "selection.pin",
        "mode.listen",
        "log.level",
    ] {
        assert!(has(&list, field), "{field}: {list:?}");
    }
    assert_eq!(list.len(), 10, "{list:?}");
}

#[test]
fn error_display_lists_every_problem() {
    let error = Config::from_toml_str("[log]\nlevel = \"loud\"\n", &Env::new()).unwrap_err();
    let Error::Invalid(list) = &error else {
        panic!("ожидались ошибки проверки: {error:?}");
    };
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].field(), "subscription");
    assert_eq!(list[1].field(), "log.level");
    assert!(list[1].message().contains("error"));

    let text = error.to_string();
    assert!(text.starts_with("ошибки в настройках:"), "{text}");
    assert!(text.contains("\n  - subscription: "), "{text}");
    assert!(text.contains("\n  - log.level: "), "{text}");
}

#[test]
fn syntax_and_type_errors() {
    let unknown = Config::from_toml_str("[[subscriptions]]\nname = \"a\"\n", &Env::new());
    let message = unknown.unwrap_err().to_string();
    assert!(message.contains("не удалось разобрать"), "{message}");
    assert!(
        message.contains("неизвестный ключ `subscriptions`"),
        "{message}"
    );

    let unknown = Config::from_toml_str(&format!("{OK_SUB}\n[log]\nlvl = \"info\"\n"), &Env::new());
    let message = unknown.unwrap_err().to_string();
    assert!(message.contains("неизвестный ключ `lvl`"), "{message}");

    let broken = Config::from_toml_str("[[subscription]\n", &Env::new());
    assert!(matches!(broken, Err(Error::Parse(_))));

    let duplicate =
        Config::from_toml_str("[log]\nlevel = \"info\"\nlevel = \"debug\"\n", &Env::new());
    assert!(matches!(duplicate, Err(Error::Parse(_))));
}

#[test]
fn type_errors_do_not_echo_values() {
    let text = "[device]\nseed = 123456789\n";
    let message = Config::from_toml_str(text, &Env::new())
        .unwrap_err()
        .to_string();
    assert!(message.contains("строка 2 (seed)"));
    assert!(
        !message.contains("123456789"),
        "значение попало в сообщение"
    );

    let text = format!("{OK_SUB}\n[selection]\nfailures = \"SECRETVALUE\"\n");
    let message = Config::from_toml_str(&text, &Env::new())
        .unwrap_err()
        .to_string();
    assert!(message.contains("(failures)"));
    assert!(
        !message.contains("SECRETVALUE"),
        "значение попало в сообщение"
    );
}

#[test]
fn oversized_files_are_refused() {
    let text = "#".repeat((1 << 20) + 1);
    let error = Config::from_toml_str(&text, &Env::new()).unwrap_err();
    assert!(matches!(error, Error::Parse(_)), "{error:?}");
}

#[test]
fn environment_without_a_file() {
    let vars = env(&[
        (
            "RAYCAT_SUBSCRIPTION",
            "https://sub.example.com/api/sub/ZzZz0001",
        ),
        ("RAYCAT_APP", "happ"),
        ("RAYCAT_PLATFORM", "android"),
        ("RAYCAT_SEED", "docker seed"),
        ("RAYCAT_MODE", "gateway"),
        ("RAYCAT_KILL_SWITCH", "off"),
        ("RAYCAT_LOG", "debug"),
    ]);
    for config in [
        Config::from_toml_str("", &vars).unwrap(),
        Config::load(None, &vars, false).unwrap(),
    ] {
        assert_eq!(config.subscriptions.len(), 1);
        let sub = &config.subscriptions[0];
        assert_eq!(sub.name, "основная");
        assert_eq!(sub.url.expose(), "https://sub.example.com/api/sub/ZzZz0001");
        assert_eq!((sub.app, sub.platform), (App::Happ, Platform::Android));
        assert_eq!(config.device.seed.as_ref().unwrap().expose(), "docker seed");
        assert_eq!(
            config.mode,
            Mode::Gateway {
                kill_switch: false,
                lan: false
            }
        );
        assert_eq!(config.log.level, LogLevel::Debug);
    }
}

#[test]
fn environment_listen_for_the_proxy() {
    let vars = env(&[
        ("RAYCAT_SUBSCRIPTION", "https://sub.example.com/x/abcd"),
        ("RAYCAT_APP", "happ"),
        ("RAYCAT_PLATFORM", "windows"),
        ("RAYCAT_LISTEN", "0.0.0.0:1080"),
    ]);
    let config = Config::from_toml_str("", &vars).unwrap();
    assert_eq!(
        config.mode,
        Mode::Proxy {
            listen: "0.0.0.0:1080".parse().unwrap()
        }
    );
}

#[test]
fn environment_overrides_the_file() {
    let vars = env(&[
        (
            "RAYCAT_SUBSCRIPTION",
            "https://other.example.org/x/ZzZz0002",
        ),
        ("RAYCAT_APP", "INCY"),
        ("RAYCAT_PLATFORM", "android"),
        ("RAYCAT_SEED", "env seed"),
        ("RAYCAT_MODE", "proxy"),
        ("RAYCAT_LISTEN", "127.0.0.1:1080"),
        ("RAYCAT_KILL_SWITCH", "true"),
        ("RAYCAT_LOG", "error"),
    ]);
    let config = Config::from_toml_str(FULL, &vars).unwrap();

    assert_eq!(config.subscriptions.len(), 2);
    let main = &config.subscriptions[0];
    assert_eq!(main.name, "основная");
    assert_eq!(main.url.expose(), "https://other.example.org/x/ZzZz0002");
    assert_eq!((main.app, main.platform), (App::Incy, Platform::Android));
    assert_eq!(main.update_interval, Some(Duration::from_secs(6 * 3_600)));
    assert_eq!(
        config.subscriptions[1].url.expose(),
        "http://203.0.113.7:8080/sub/reserve-token-9876"
    );

    assert_eq!(config.device.seed.as_ref().unwrap().expose(), "env seed");
    assert_eq!(config.device.hostname.as_deref(), Some("DESKTOP-TEST01"));
    assert_eq!(
        config.mode,
        Mode::Proxy {
            listen: "127.0.0.1:1080".parse().unwrap()
        }
    );
    assert_eq!(config.log.level, LogLevel::Error);
    assert_eq!(config.xray.memory_limit, 64 << 20);
}

#[test]
fn environment_kill_switch_over_the_file() {
    let file = format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\nkill_switch = true\n");
    let config = Config::from_toml_str(&file, &env(&[("RAYCAT_KILL_SWITCH", "0")])).unwrap();
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: false,
            lan: false
        }
    );
}

#[test]
fn environment_seed_conflicts_with_the_file_machine_id() {
    let file = format!("[device]\nmachine_id = \"00000000000000000000000000000000\"\n{OK_SUB}");
    let list = problems_with_env(&file, &env(&[("RAYCAT_SEED", "env seed")]));
    assert_eq!(
        list,
        ["device: RAYCAT_SEED нельзя задавать вместе с device.machine_id из файла настроек"]
    );
}

#[test]
fn file_machine_id_is_kept_without_environment_seed() {
    let file = format!("[device]\nmachine_id = \"00000000000000000000000000000000\"\n{OK_SUB}");
    let config = Config::from_toml_str(&file, &env(&[("RAYCAT_LOG", "debug")])).unwrap();
    assert!(config.device.machine_id.is_some());
    assert!(config.device.seed.is_none());
}

#[test]
fn empty_variables_are_ignored() {
    let file = format!("{OK_SUB}\n[mode]\ntype = \"gateway\"\n");
    let vars = env(&[
        ("RAYCAT_KILL_SWITCH", ""),
        ("RAYCAT_APP", ""),
        ("RAYCAT_LOG", ""),
        ("RAYCAT_SEED", ""),
    ]);
    let config = Config::from_toml_str(&file, &vars).unwrap();
    assert_eq!(
        config.mode,
        Mode::Gateway {
            kill_switch: true,
            lan: false
        }
    );
    assert_eq!(config.subscriptions[0].app, App::Happ);
    assert!(config.device.seed.is_none());

    let list = problems_with_env("", &env(&[("RAYCAT_SUBSCRIPTION", "")]));
    assert!(has(&list, "subscription"), "{list:?}");
}

#[test]
fn environment_errors_name_the_variable() {
    let vars = env(&[
        ("RAYCAT_SUBSCRIPTION", "https://sub.example.com/x/ZzZz0003"),
        ("RAYCAT_APP", "firefox"),
        ("RAYCAT_PLATFORM", "linux"),
        ("RAYCAT_MODE", "vpn"),
        ("RAYCAT_LISTEN", "nowhere"),
        ("RAYCAT_KILL_SWITCH", "maybe"),
        ("RAYCAT_LOG", "trace"),
    ]);
    let list = problems_with_env("", &vars);
    for key in [
        "RAYCAT_APP",
        "RAYCAT_PLATFORM",
        "RAYCAT_MODE",
        "RAYCAT_LISTEN",
        "RAYCAT_KILL_SWITCH",
        "RAYCAT_LOG",
    ] {
        assert!(has(&list, key), "нет ошибки для {key}");
    }
    assert!(has(&list, "subscription[0].app"));
    assert!(has(&list, "subscription[0].platform"));
    assert!(
        list.iter().all(|line| !line.contains("ZzZz0003")),
        "ссылка попала в сообщение"
    );
}

#[test]
fn environment_subscription_is_checked_and_masked() {
    let vars = env(&[
        ("RAYCAT_SUBSCRIPTION", "http://sub.example.com/x/ZzZz0004"),
        ("RAYCAT_APP", "incy"),
        ("RAYCAT_PLATFORM", "windows"),
    ]);
    let list = problems_with_env("", &vars);
    assert_eq!(list.len(), 2);
    assert!(has(&list, "subscription[0].url"));
    assert!(has(&list, "subscription[0].platform"));
    assert!(
        list.iter().all(|line| !line.contains("ZzZz0004")),
        "ссылка не замаскирована"
    );
    assert!(list[0].contains("…0004"));
}

#[test]
fn environment_without_app_and_platform_asks_for_them() {
    let vars = env(&[("RAYCAT_SUBSCRIPTION", "https://sub.example.com/x/abcd")]);
    let list = problems_with_env("", &vars);
    assert_eq!(list.len(), 2, "{list:?}");
    assert!(has(&list, "subscription[0].app"), "{list:?}");
    assert!(has(&list, "subscription[0].platform"), "{list:?}");
}

#[test]
fn loads_a_file() {
    let path = temp_file("explicit", FULL.as_bytes());
    let config = Config::load(Some(&path), &Env::new(), false).unwrap();
    assert_eq!(config.subscriptions.len(), 2);

    let vars = env(&[
        ("RAYCAT_CONFIG", path.to_str().unwrap()),
        ("RAYCAT_LOG", "warn"),
    ]);
    let config = Config::load(None, &vars, false).unwrap();
    assert_eq!(config.subscriptions.len(), 2);
    assert_eq!(config.log.level, LogLevel::Warn);

    let missing = env(&[("RAYCAT_CONFIG", "/nonexistent/raycat.toml")]);
    let config = Config::load(Some(&path), &missing, false).unwrap();
    assert_eq!(config.subscriptions.len(), 2);

    std::fs::remove_file(path).unwrap();
}

#[test]
fn file_errors() {
    let missing = std::env::temp_dir().join("raycat-config-test-missing-file.toml");
    let error = Config::load(Some(&missing), &Env::new(), false).unwrap_err();
    assert!(matches!(error, Error::Read { .. }), "{error:?}");
    assert!(error.to_string().contains("не удалось прочитать"));

    let error = Config::load(
        None,
        &env(&[("RAYCAT_CONFIG", "/nonexistent/raycat.toml")]),
        true,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Read { .. }), "{error:?}");

    let binary = temp_file("binary", &[0xff, 0xfe, 0x00, 0x80]);
    let error = Config::load(Some(&binary), &Env::new(), false).unwrap_err();
    assert!(matches!(error, Error::Read { .. }), "{error:?}");
    std::fs::remove_file(binary).unwrap();

    let broken = temp_file("broken", b"[[subscription]\n");
    let error = Config::load(Some(&broken), &Env::new(), false).unwrap_err();
    assert!(matches!(error, Error::Parse(_)), "{error:?}");
    std::fs::remove_file(broken).unwrap();
}

#[test]
fn debug_output_hides_secrets() {
    let config = parse(&format!(
        "[device]\nmachine_id = \"0123456789abcdef0123456789abcdef\"\n{}",
        FULL.split_once("[[subscription]]")
            .map(|(_, rest)| format!("[[subscription]]{rest}"))
            .unwrap()
    ));
    let debug = format!("{config:?}");
    for (i, secret) in [
        "AbCdEfGh1234",
        "sub.example.com/api",
        "reserve-token-9876",
        "другое устройство",
        "0123456789abcdef0123456789abcdef",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(!debug.contains(secret), "секрет №{i} попал в Debug");
    }
    assert!(debug.contains("Secret(***)"));
    assert!(debug.contains("основная"));
}

#[test]
fn seed_is_hidden_in_debug() {
    let config = parse(FULL);
    let debug = format!("{config:?}");
    assert!(!debug.contains("любая фраза"), "seed попал в Debug");
    assert!(!debug.contains("AbCdEfGh1234"), "ссылка попала в Debug");
    assert!(!format!("{:?}", config.subscriptions[0]).contains("sub.example.com"));
}

fn url_file_config(path: &Path) -> String {
    format!(
        "[[subscription]]\nname = \"основная\"\nurl_file = '{}'\napp = \"happ\"\nplatform = \"windows\"\n",
        path.display()
    )
}

#[test]
fn url_file_is_read_and_trimmed() {
    let path = temp_file(
        "url-file-trim",
        "  https://sub.example.com/api/sub/FileTok0001 \r\n\n".as_bytes(),
    );
    let config = Config::from_toml_str(&url_file_config(&path), &Env::new()).unwrap();
    assert_eq!(
        config.subscriptions[0].url.expose(),
        "https://sub.example.com/api/sub/FileTok0001"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn url_file_errors_name_the_file_and_mask_the_link() {
    for (name, content, reason, tail) in [
        (
            "url-file-space",
            "https://sub.example.com/a b/FileSecret0002\n",
            "пробелы",
            "…0002",
        ),
        (
            "url-file-http",
            "http://sub.example.com/x/FileSecret0003",
            "http небезопасен",
            "…0003",
        ),
    ] {
        let path = temp_file(name, content.as_bytes());
        let list = problems(&url_file_config(&path));
        assert_eq!(list.len(), 1, "{list:?}");
        assert!(has(&list, "subscription[0].url_file"), "{list:?}");
        assert!(list[0].contains(&path.display().to_string()), "{list:?}");
        assert!(list[0].contains(reason), "{list:?}");
        assert!(list[0].contains(tail), "{list:?}");
        assert!(
            !list[0].contains("FileSecret"),
            "ссылка не замаскирована: {list:?}"
        );
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn empty_url_file_is_refused() {
    let path = temp_file("url-file-empty", " \n\t\n".as_bytes());
    let list = problems(&url_file_config(&path));
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(has(&list, "subscription[0].url_file"), "{list:?}");
    assert!(list[0].ends_with("пустой"), "{list:?}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn missing_url_file_is_refused() {
    let path = std::env::temp_dir().join("raycat-config-test-missing-url-file");
    let list = problems(&url_file_config(&path));
    assert_eq!(list.len(), 1, "{list:?}");
    assert!(
        list[0].starts_with("subscription[0].url_file: не удалось прочитать "),
        "{list:?}"
    );
    assert!(list[0].contains(&path.display().to_string()), "{list:?}");
}

#[test]
fn url_file_size_is_limited_to_4_kib() {
    let link = "https://sub.example.com/api/sub/FileTok0005";
    let padded = format!("{link}{}", " ".repeat(4096 - link.len()));
    let path = temp_file("url-file-4kib", padded.as_bytes());
    let config = Config::from_toml_str(&url_file_config(&path), &Env::new()).unwrap();
    assert_eq!(config.subscriptions[0].url.expose(), link);
    std::fs::remove_file(path).unwrap();

    let path = temp_file("url-file-too-big", " ".repeat(4097).as_bytes());
    let list = problems(&url_file_config(&path));
    assert!(has(&list, "subscription[0].url_file"), "{list:?}");
    assert!(list[0].contains("больше 4 КиБ"), "{list:?}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn url_and_url_file_are_mutually_exclusive() {
    let file = format!("{OK_SUB}url_file = \"/run/secrets/raycat_sub\"\n");
    assert_eq!(
        problems(&file),
        ["subscription[0]: укажите url или url_file (только одно)"]
    );
}

#[test]
fn url_or_url_file_is_required() {
    let list = problems("[[subscription]]\nname = \"a\"\napp = \"happ\"\nplatform = \"windows\"\n");
    assert_eq!(
        list,
        ["subscription[0]: укажите url или url_file (только одно)"]
    );
}

#[test]
fn subscription_file_environment_creates_the_first_subscription() {
    let path = temp_file(
        "url-file-env",
        b"https://sub.example.com/api/sub/FileTok0006\n",
    );
    let vars = env(&[
        ("RAYCAT_SUBSCRIPTION_FILE", path.to_str().unwrap()),
        ("RAYCAT_APP", "happ"),
        ("RAYCAT_PLATFORM", "windows"),
    ]);
    let config = Config::from_toml_str("", &vars).unwrap();
    assert_eq!(config.subscriptions.len(), 1);
    assert_eq!(config.subscriptions[0].name, "основная");
    assert_eq!(
        config.subscriptions[0].url.expose(),
        "https://sub.example.com/api/sub/FileTok0006"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn subscription_variables_replace_the_file_link() {
    let path = temp_file(
        "url-file-override",
        b"https://sub.example.com/api/sub/FileTok0007\n",
    );
    let vars = env(&[("RAYCAT_SUBSCRIPTION_FILE", path.to_str().unwrap())]);
    let config = Config::from_toml_str(OK_SUB, &vars).unwrap();
    assert_eq!(
        config.subscriptions[0].url.expose(),
        "https://sub.example.com/api/sub/FileTok0007"
    );

    let vars = env(&[(
        "RAYCAT_SUBSCRIPTION",
        "https://sub.example.com/x/EnvTok0008",
    )]);
    let config = Config::from_toml_str(&url_file_config(&path), &vars).unwrap();
    assert_eq!(
        config.subscriptions[0].url.expose(),
        "https://sub.example.com/x/EnvTok0008"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn subscription_file_environment_conflicts_with_subscription() {
    let vars = env(&[
        ("RAYCAT_SUBSCRIPTION", "https://sub.example.com/x/abcd"),
        ("RAYCAT_SUBSCRIPTION_FILE", "/run/secrets/raycat_sub"),
        ("RAYCAT_APP", "happ"),
        ("RAYCAT_PLATFORM", "windows"),
    ]);
    assert_eq!(
        problems_with_env("", &vars),
        ["RAYCAT_SUBSCRIPTION_FILE: нельзя задавать вместе с RAYCAT_SUBSCRIPTION"]
    );
}
