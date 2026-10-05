use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// Сеть в виде «адрес/длина префикса». Биты хоста должны быть нулевыми: `nft` в
/// интервальных множествах не принимает `10.1.2.3/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidrError {
    Syntax,
    Prefix { max: u8 },
    HostBits,
}

impl fmt::Display for CidrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("ожидался адрес или сеть вида 10.0.0.0/8"),
            Self::Prefix { max } => write!(f, "длина префикса должна быть от 0 до {max}"),
            Self::HostBits => {
                f.write_str("в адресе сети заданы биты хоста (нужно 10.0.0.0/8, а не 10.1.2.3/8)")
            }
        }
    }
}

impl Error for CidrError {}

fn bits(addr: IpAddr) -> u128 {
    match addr {
        IpAddr::V4(addr) => u128::from(u32::from(addr)) << 96,
        IpAddr::V6(addr) => u128::from(addr),
    }
}

/// Маска для IPv4 считается в старших 32 битах, как и сами адреса в `bits`.
fn mask(prefix: u8) -> u128 {
    match prefix {
        0 => 0,
        prefix => u128::MAX << (128 - u32::from(prefix)),
    }
}

impl Cidr {
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, CidrError> {
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(CidrError::Prefix { max });
        }
        if bits(addr) & !mask(prefix) != 0 {
            return Err(CidrError::HostBits);
        }
        Ok(Self { addr, prefix })
    }

    /// Сеть, которой принадлежит адрес: биты хоста обнуляются (`192.168.1.10/24` →
    /// `192.168.1.0/24`).
    pub fn containing(addr: IpAddr, prefix: u8) -> Result<Self, CidrError> {
        let network = match addr {
            IpAddr::V4(v4) => {
                if prefix > 32 {
                    return Err(CidrError::Prefix { max: 32 });
                }
                let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
                IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
            }
            IpAddr::V6(v6) => {
                if prefix > 128 {
                    return Err(CidrError::Prefix { max: 128 });
                }
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask(prefix)))
            }
        };
        Self::new(network, prefix)
    }

    pub(crate) const fn v4(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> Self {
        Self {
            addr: IpAddr::V4(Ipv4Addr::new(a, b, c, d)),
            prefix,
        }
    }

    pub(crate) const fn v6(first: u16, prefix: u8) -> Self {
        Self {
            addr: IpAddr::V6(Ipv6Addr::new(first, 0, 0, 0, 0, 0, 0, 0)),
            prefix,
        }
    }

    pub const fn addr(&self) -> IpAddr {
        self.addr
    }

    pub const fn prefix(&self) -> u8 {
        self.prefix
    }

    pub const fn is_ipv4(&self) -> bool {
        self.addr.is_ipv4()
    }

    pub(crate) fn contains(&self, other: &Self) -> bool {
        self.is_ipv4() == other.is_ipv4()
            && self.prefix <= other.prefix
            && bits(other.addr) & mask(self.prefix) == bits(self.addr)
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl FromStr for Cidr {
    type Err = CidrError;

    /// Адрес без длины префикса означает один хост (`/32` или `/128`).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr.parse().map_err(|_| CidrError::Syntax)?;
        let prefix = match prefix {
            Some(prefix) => {
                if prefix.is_empty() || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(CidrError::Syntax);
                }
                prefix.parse().map_err(|_| CidrError::Prefix {
                    max: if addr.is_ipv4() { 32 } else { 128 },
                })?
            }
            None if addr.is_ipv4() => 32,
            None => 128,
        };
        Self::new(addr, prefix)
    }
}

/// Сортирует, убирает повторы и сети, целиком лежащие в других: `nft` отвергает
/// пересекающиеся элементы интервального множества.
pub(crate) fn normalize(nets: impl IntoIterator<Item = Cidr>) -> Vec<Cidr> {
    let mut nets: Vec<Cidr> = nets.into_iter().collect();
    nets.sort_unstable();
    nets.dedup();
    let all = nets.clone();
    nets.retain(|net| !all.iter().any(|other| other != net && other.contains(net)));
    nets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(text: &str) -> Cidr {
        text.parse().unwrap()
    }

    #[test]
    fn parses_and_prints_networks() {
        assert_eq!(cidr("10.0.0.0/8").to_string(), "10.0.0.0/8");
        assert_eq!(cidr("fc00::/7").to_string(), "fc00::/7");
        assert_eq!(cidr("2001:db8::/32").to_string(), "2001:db8::/32");
        assert_eq!(cidr("203.0.113.7").to_string(), "203.0.113.7/32");
        assert_eq!(cidr("::1").to_string(), "::1/128");
        assert_eq!(cidr("0.0.0.0/0").prefix(), 0);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!("".parse::<Cidr>(), Err(CidrError::Syntax));
        assert_eq!("example.com".parse::<Cidr>(), Err(CidrError::Syntax));
        assert_eq!("10.0.0.0/".parse::<Cidr>(), Err(CidrError::Syntax));
        assert_eq!("10.0.0.0/+8".parse::<Cidr>(), Err(CidrError::Syntax));
        assert_eq!("10.0.0.0/8/8".parse::<Cidr>(), Err(CidrError::Syntax));
        assert_eq!(
            "10.0.0.0/33".parse::<Cidr>(),
            Err(CidrError::Prefix { max: 32 })
        );
        assert_eq!(
            "::/129".parse::<Cidr>(),
            Err(CidrError::Prefix { max: 128 })
        );
        assert_eq!(
            "10.0.0.0/999".parse::<Cidr>(),
            Err(CidrError::Prefix { max: 32 })
        );
        assert_eq!("10.1.2.3/8".parse::<Cidr>(), Err(CidrError::HostBits));
        assert_eq!("fc00::1/7".parse::<Cidr>(), Err(CidrError::HostBits));
    }

    #[test]
    fn containing_clears_the_host_bits() {
        let of = |text: &str, prefix| Cidr::containing(text.parse().unwrap(), prefix).unwrap();
        assert_eq!(of("192.168.1.10", 24).to_string(), "192.168.1.0/24");
        assert_eq!(of("10.77.0.2", 8).to_string(), "10.0.0.0/8");
        assert_eq!(of("10.77.0.2", 32).to_string(), "10.77.0.2/32");
        assert_eq!(of("10.77.0.2", 0).to_string(), "0.0.0.0/0");
        assert_eq!(of("fd00:1:2::5", 48).to_string(), "fd00:1:2::/48");
        assert_eq!(
            Cidr::containing("10.0.0.1".parse().unwrap(), 33),
            Err(CidrError::Prefix { max: 32 })
        );
        assert_eq!(
            Cidr::containing("::1".parse().unwrap(), 129),
            Err(CidrError::Prefix { max: 128 })
        );
    }

    #[test]
    fn contains_only_same_family_and_narrower_networks() {
        assert!(cidr("10.0.0.0/8").contains(&cidr("10.1.0.0/16")));
        assert!(cidr("10.0.0.0/8").contains(&cidr("10.0.0.0/8")));
        assert!(!cidr("10.1.0.0/16").contains(&cidr("10.0.0.0/8")));
        assert!(!cidr("10.0.0.0/8").contains(&cidr("11.0.0.0/8")));
        assert!(cidr("fc00::/7").contains(&cidr("fd00::/8")));
        assert!(!cidr("0.0.0.0/0").contains(&cidr("::/0")));
        assert!(cidr("0.0.0.0/0").contains(&cidr("203.0.113.0/24")));
    }

    #[test]
    fn normalize_sorts_and_drops_nested_and_duplicates() {
        let nets = normalize([
            cidr("192.168.0.0/16"),
            cidr("10.1.0.0/16"),
            cidr("10.0.0.0/8"),
            cidr("192.168.0.0/16"),
            cidr("fd00::/8"),
            cidr("fc00::/7"),
            cidr("192.168.1.0/24"),
        ]);
        let text: Vec<String> = nets.iter().map(ToString::to_string).collect();
        assert_eq!(text, ["10.0.0.0/8", "192.168.0.0/16", "fc00::/7"]);
    }
}
