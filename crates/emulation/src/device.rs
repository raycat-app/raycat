//! Эмулируемое устройство и алгоритмы вывода его идентификаторов из seed.

use ring::digest;

/// Данные устройства. Всё, что не задано, берётся из профиля или выводится из
/// `machine_id`, поэтому одна установка остаётся одним устройством.
#[derive(Debug, Clone, Default)]
pub struct Device {
    /// Источник всех идентификаторов; не должен быть пустым.
    pub machine_id: String,
    /// Имя компьютера Windows (по умолчанию `DESKTOP-XXXXXXX` из `machine_id`).
    pub hostname: Option<String>,
    /// Заменяет `X-Device-Model` целиком.
    pub model: Option<String>,
    pub os_version: Option<String>,
    /// Готовый HWID вместо выводимого из `machine_id`.
    pub hwid: Option<String>,
    /// Локаль системы в стиле POSIX: `ru_RU.UTF-8`, `ru`, `en`, `C`.
    pub locale: String,
}

impl Device {
    /// Устройство, чей `machine_id` выводится из seed из настроек.
    pub fn from_seed(seed: &str) -> Self {
        Self::from_machine_id(machine_id_from_seed(seed))
    }

    pub fn from_machine_id(machine_id: impl Into<String>) -> Self {
        Self {
            machine_id: machine_id.into(),
            ..Self::default()
        }
    }
}

/// Значения, которыми эмулируемое устройство представляется провайдеру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub hwid: String,
    pub os: String,
    pub os_version: String,
    pub model: String,
}

/// Какой идентификатор шлёт платформа как HWID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HwidAlgorithm {
    /// `MachineGuid` из реестра Windows.
    WindowsMachineGuid,
    /// `ANDROID_ID` приложения.
    AndroidId,
}

impl HwidAlgorithm {
    pub(crate) const NAMES: &'static str = "windows-machine-guid, android-id";

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "windows-machine-guid" => Some(Self::WindowsMachineGuid),
            "android-id" => Some(Self::AndroidId),
            _ => None,
        }
    }

    pub(crate) fn derive(self, machine_id: &str) -> String {
        match self {
            Self::WindowsMachineGuid => windows_machine_guid(machine_id),
            Self::AndroidId => android_id(machine_id),
        }
    }
}

/// `machine_id` в формате systemd (32 шестнадцатеричные цифры), которому
/// соответствует seed. Префикс отделяет его от любого другого использования той
/// же строки.
pub fn machine_id_from_seed(seed: &str) -> String {
    let input = format!("raycat device seed\0{}", seed.trim());
    sha256_hex(input.as_bytes())[..32].to_owned()
}

/// Регулярка Remnawave `^[a-zA-Z0-9=-]{10,64}$`: остальные HWID панель молча
/// считает отсутствующими.
pub fn is_valid_hwid(hwid: &str) -> bool {
    (10..=64).contains(&hwid.len())
        && hwid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'=' || b == b'-')
}

/// `MachineGuid` Windows: UUID версии 4 в нижнем регистре.
pub(crate) fn windows_machine_guid(machine_id: &str) -> String {
    let hex = sha256_hex(format!("windows machine guid\0{machine_id}").as_bytes());
    let variant = char::from(b"89ab"[usize::from(hex.as_bytes()[16] & 3)]);
    format!(
        "{}-{}-4{}-{variant}{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// Имя компьютера Windows по умолчанию: `DESKTOP-` и 7 символов.
pub(crate) fn windows_computer_name(machine_id: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let hash = sha256(format!("windows computer name\0{machine_id}").as_bytes());
    let suffix: String = hash.as_ref()[..7]
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect();
    format!("DESKTOP-{suffix}")
}

/// `ANDROID_ID`: 16 шестнадцатеричных цифр в нижнем регистре.
pub(crate) fn android_id(machine_id: &str) -> String {
    sha256_hex(format!("android id\0{machine_id}").as_bytes())[..16].to_owned()
}

fn sha256(data: &[u8]) -> digest::Digest {
    digest::digest(&digest::SHA256, data)
}

fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in sha256(data).as_ref() {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE_ID: &str = "0d0af05ee8fd4dc29275718f2ce4dff1";

    fn is_hex(s: &str) -> bool {
        s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn seed_gives_a_stable_distinct_machine_id() {
        let id = machine_id_from_seed("my home server");
        assert_eq!(id.len(), 32);
        assert!(is_hex(&id));
        assert_eq!(id, machine_id_from_seed("  my home server\n"));
        assert_ne!(id, machine_id_from_seed("my home server 2"));
        // Смена вывода переселила бы всех пользователей с seed на новое устройство.
        assert_eq!(
            machine_id_from_seed("seed"),
            sha256_hex(b"raycat device seed\0seed")[..32]
        );
        assert_eq!(
            Device::from_seed("seed").machine_id,
            machine_id_from_seed("seed")
        );
    }

    #[test]
    fn windows_machine_guid_is_a_version_4_uuid() {
        let guid = windows_machine_guid(MACHINE_ID);
        let groups: Vec<&str> = guid.split('-').collect();
        let lengths: Vec<usize> = guid.split('-').map(str::len).collect();
        assert_eq!(lengths, [8, 4, 4, 4, 12]);
        assert!(groups[2].starts_with('4'), "версия 4: {guid}");
        assert!(matches!(groups[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        assert!(is_hex(&guid.replace('-', "")));
        assert!(is_valid_hwid(&guid));
    }

    #[test]
    fn windows_machine_guid_is_deterministic_per_machine() {
        assert_eq!(
            windows_machine_guid(MACHINE_ID),
            windows_machine_guid(MACHINE_ID)
        );
        assert_ne!(
            windows_machine_guid(MACHINE_ID),
            windows_machine_guid("11112222333344445555666677778888")
        );
    }

    #[test]
    fn windows_computer_name_looks_like_the_default_one() {
        let name = windows_computer_name(MACHINE_ID);
        assert_eq!(name.len(), 15);
        let suffix = name.strip_prefix("DESKTOP-").unwrap();
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        );
        assert_eq!(name, windows_computer_name(MACHINE_ID));
        assert_ne!(
            name,
            windows_computer_name("11112222333344445555666677778888")
        );
    }

    #[test]
    fn android_id_is_16_lowercase_hex_digits() {
        let id = android_id(MACHINE_ID);
        assert_eq!(id.len(), 16);
        assert!(is_hex(&id));
        assert!(is_valid_hwid(&id));
        assert_eq!(id, android_id(MACHINE_ID));
        assert_ne!(id, android_id("11112222333344445555666677778888"));
    }

    #[test]
    fn hwid_is_validated_like_remnawave() {
        assert!(is_valid_hwid("A3B522EAA6F7DD89"));
        assert!(is_valid_hwid("a3ee4d8e-7e6c-4c32-9490-f157ef0ceea8"));
        assert!(is_valid_hwid("abc=abc=abc="));
        assert!(is_valid_hwid(&"a".repeat(10)));
        assert!(is_valid_hwid(&"a".repeat(64)));
        assert!(!is_valid_hwid(&"a".repeat(9)));
        assert!(!is_valid_hwid(&"a".repeat(65)));
        assert!(!is_valid_hwid("has_underscore_1234"));
        assert!(!is_valid_hwid("пробел не подходит"));
        assert!(!is_valid_hwid(""));
    }

    #[test]
    fn algorithms_are_selected_by_name() {
        assert_eq!(
            HwidAlgorithm::from_name("windows-machine-guid"),
            Some(HwidAlgorithm::WindowsMachineGuid)
        );
        assert_eq!(
            HwidAlgorithm::from_name("android-id"),
            Some(HwidAlgorithm::AndroidId)
        );
        assert_eq!(HwidAlgorithm::from_name("raw"), None);
        assert_eq!(
            HwidAlgorithm::AndroidId.derive(MACHINE_ID),
            android_id(MACHINE_ID)
        );
        assert_eq!(
            HwidAlgorithm::WindowsMachineGuid.derive(MACHINE_ID),
            windows_machine_guid(MACHINE_ID)
        );
    }
}
