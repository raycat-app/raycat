use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};

use anyhow::{Result, anyhow, bail};

use crate::cidr::{Cidr, normalize};
use crate::exec::{Executor, System};
use crate::rules::{Lan, interface_name_problem, lan_subnet_problem};

const ROUTES: &str = "/proc/net/route";
const DEVICES: &str = "/proc/net/dev";
const MAX_PROC_FILE: u64 = 256 * 1024;
const RTF_UP: u32 = 1;

/// Интерфейс и подсети устройств локальной сети. Что не задано, определяется по
/// системе: интерфейс — по маршруту по умолчанию, подсети — по адресам интерфейса.
pub fn resolve_lan(interface: Option<&str>, subnets: &[Cidr]) -> Result<Lan> {
    resolve_with(
        &System,
        interface,
        subnets,
        &read_proc(ROUTES),
        &read_proc(DEVICES),
    )
}

fn read_proc(path: &str) -> String {
    let mut text = String::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(MAX_PROC_FILE).read_to_string(&mut text);
    }
    text
}

fn resolve_with(
    exec: &dyn Executor,
    interface: Option<&str>,
    subnets: &[Cidr],
    routes: &str,
    devices: &str,
) -> Result<Lan> {
    let interface = match interface {
        Some(name) => name.to_owned(),
        None => default_interface(routes).ok_or_else(|| {
            anyhow!(
                "не удалось определить интерфейс локальной сети: в системе нет маршрута IPv4 \
                 по умолчанию (задайте lan_interface)"
            )
        })?,
    };
    if let Some(problem) = interface_name_problem(&interface) {
        bail!("интерфейс «{interface}»: {problem}");
    }
    if !device_names(devices).any(|name| name == interface) {
        bail!("сетевого интерфейса «{interface}» нет в системе (список: `ip link`)");
    }
    let subnets = if subnets.is_empty() {
        interface_subnets(exec, &interface)?
    } else {
        subnets.to_vec()
    };
    Ok(Lan { interface, subnets })
}

/// Интерфейс активного маршрута по умолчанию с наименьшей метрикой.
fn default_interface(routes: &str) -> Option<String> {
    routes
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [name, destination, _, flags, _, _, metric, mask, ..] = fields.as_slice() else {
                return None;
            };
            let up = u32::from_str_radix(flags, 16).ok()? & RTF_UP != 0;
            let default = *destination == "00000000" && *mask == "00000000";
            (up && default).then(|| {
                (
                    metric.parse::<u32>().unwrap_or(u32::MAX),
                    (*name).to_owned(),
                )
            })
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, name)| name)
}

fn device_names(devices: &str) -> impl Iterator<Item = &str> {
    devices
        .lines()
        .skip(2)
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
}

fn interface_subnets(exec: &dyn Executor, interface: &str) -> Result<Vec<Cidr>> {
    let output = exec.run("ip", &["-4", "-o", "addr", "show", "dev", interface], None)?;
    if !output.success {
        bail!(
            "не удалось узнать адреса интерфейса «{interface}»: {}",
            output.stderr
        );
    }
    let subnets = parse_subnets(&output.stdout);
    if subnets.is_empty() {
        bail!(
            "у интерфейса «{interface}» нет подходящего адреса IPv4: задайте подсети \
             устройств (lan_subnets)"
        );
    }
    Ok(subnets)
}

/// Подсети из вывода `ip -4 -o addr show`: глобальные адреса, кроме одиночных
/// (`/32`, точка-точка) и слишком крупных сетей.
fn parse_subnets(listing: &str) -> Vec<Cidr> {
    let mut nets = Vec::new();
    for line in listing.lines() {
        let mut tokens = line.split_whitespace();
        let mut address = None;
        let mut scope = None;
        while let Some(token) = tokens.next() {
            match token {
                "inet" => address = tokens.next(),
                "scope" => scope = tokens.next(),
                _ => {}
            }
        }
        if scope != Some("global") {
            continue;
        }
        let Some((addr, prefix)) = address.and_then(|text| text.split_once('/')) else {
            continue;
        };
        let (Ok(addr), Ok(prefix)) = (addr.parse::<Ipv4Addr>(), prefix.parse::<u8>()) else {
            continue;
        };
        if let Ok(net) = Cidr::containing(IpAddr::V4(addr), prefix)
            && lan_subnet_problem(&net).is_none()
        {
            nets.push(net);
        }
    }
    normalize(nets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;

    const ROUTES: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
        docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";

    const DEVICES: &str = "Inter-|   Receive                                                |  Transmit\n \
         face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
         lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0\n  \
         eth0: 5000 50 0 0 0 0 0 0 5000 50 0 0 0 0 0 0\n\
         br-lan.10: 5000 50 0 0 0 0 0 0 5000 50 0 0 0 0 0 0\n";

    const ADDRESSES: &str = "2: eth0    inet 192.168.1.10/24 brd 192.168.1.255 scope global dynamic eth0\\       valid_lft 86000sec preferred_lft 86000sec\n\
        2: eth0    inet 10.20.30.4/16 brd 10.20.255.255 scope global secondary eth0\\       valid_lft forever preferred_lft forever\n\
        2: eth0    inet 169.254.3.4/16 brd 169.254.255.255 scope link eth0\\       valid_lft forever preferred_lft forever\n\
        2: eth0    inet 10.99.0.9/32 scope global eth0\\       valid_lft forever preferred_lft forever\n";

    struct Fake(Output);

    impl Executor for Fake {
        fn run(&self, _: &str, _: &[&str], _: Option<&str>) -> Result<Output> {
            Ok(self.0.clone())
        }
    }

    fn listing(stdout: &str) -> Fake {
        Fake(Output {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    #[test]
    fn the_default_interface_has_the_lowest_metric() {
        assert_eq!(default_interface(ROUTES).as_deref(), Some("eth0"));
    }

    #[test]
    fn no_default_route_means_no_interface() {
        let only_local = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
            eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(default_interface(only_local), None);
        assert_eq!(default_interface(""), None);
        let down = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
            eth0\t00000000\t0101A8C0\t0002\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(default_interface(down), None);
    }

    #[test]
    fn device_names_come_from_the_second_header_line_on() {
        let names: Vec<&str> = device_names(DEVICES).collect();
        assert_eq!(names, ["lo", "eth0", "br-lan.10"]);
    }

    #[test]
    fn subnets_come_from_global_addresses_only() {
        let nets = parse_subnets(ADDRESSES);
        let text: Vec<String> = nets.iter().map(ToString::to_string).collect();
        assert_eq!(text, ["10.20.0.0/16", "192.168.1.0/24"]);
    }

    #[test]
    fn everything_is_detected_when_nothing_is_given() {
        let fake = listing(ADDRESSES);
        let lan = resolve_with(&fake, None, &[], ROUTES, DEVICES).unwrap();
        assert_eq!(lan.interface, "eth0");
        assert_eq!(lan.subnets.len(), 2);
    }

    #[test]
    fn given_values_win_and_skip_the_address_lookup() {
        let fake = Fake(Output {
            success: false,
            stdout: String::new(),
            stderr: "не должно вызываться".to_owned(),
        });
        let given: Vec<Cidr> = vec!["10.77.0.0/24".parse().unwrap()];
        let lan = resolve_with(&fake, Some("br-lan.10"), &given, "", DEVICES).unwrap();
        assert_eq!(lan.interface, "br-lan.10");
        assert_eq!(lan.subnets, given);
    }

    #[test]
    fn problems_are_explained_in_russian() {
        let ok = listing(ADDRESSES);
        let error = resolve_with(&ok, None, &[], "", DEVICES).unwrap_err();
        assert!(error.to_string().contains("lan_interface"), "{error}");

        let error = resolve_with(&ok, Some("eth9"), &[], ROUTES, DEVICES).unwrap_err();
        assert!(error.to_string().contains("нет в системе"), "{error}");

        let error = resolve_with(&ok, Some("eth0\" drop"), &[], ROUTES, DEVICES).unwrap_err();
        assert!(error.to_string().contains("допустимы"), "{error}");

        let empty = listing("");
        let error = resolve_with(&empty, Some("eth0"), &[], ROUTES, DEVICES).unwrap_err();
        assert!(error.to_string().contains("lan_subnets"), "{error}");

        let broken = Fake(Output {
            success: false,
            stdout: String::new(),
            stderr: "Device does not exist".to_owned(),
        });
        let error = resolve_with(&broken, Some("eth0"), &[], ROUTES, DEVICES).unwrap_err();
        assert!(
            error.to_string().contains("Device does not exist"),
            "{error}"
        );
    }
}
