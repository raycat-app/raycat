//! Каталог состояния. Каталоги 0700, файлы 0600, запись через временный файл и
//! `rename`: читатель видит либо старое содержимое, либо новое целиком.
//!
//! ```text
//! <состояние>/
//!   machine-id                    идентификатор устройства, если он не задан в настройках
//!   xray.json                     конфиг работающего xray
//!   subscriptions/<ключ>/body     последний рабочий ответ подписки
//!   subscriptions/<ключ>/state.json
//! ```

use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::log::warn;
use crate::util::fnv1a;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MACHINE_ID_FILE: &str = "machine-id";

/// Что известно о подписке между запусками.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SubState {
    /// Отпечаток ссылки и приложения: ответ годится только для тех же настроек.
    pub(crate) source: String,
    /// Статус и заголовки последнего рабочего ответа.
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    /// Unix-время получения рабочего ответа; 0 — кэша нет.
    pub(crate) fetched_at: u64,
    pub(crate) next_update: u64,
    pub(crate) last_error: Option<String>,
    /// Адрес, на который переехал провайдер (`new-url`, `new-domain`).
    pub(crate) replaced_url: Option<String>,
    pub(crate) fallback_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub(crate) fn open(root: PathBuf) -> Result<Self> {
        create_private_dir(&root)?;
        create_private_dir(&root.join("subscriptions"))?;
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(crate) fn write_file(&self, name: &str, data: &[u8]) -> Result<()> {
        write_atomic(&self.path(name), data)
    }

    /// Идентификатор устройства для подписок без `seed`; создаётся при первом обращении
    /// и больше не меняется.
    pub(crate) fn machine_id(&self) -> Result<String> {
        let path = self.path(MACHINE_ID_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => checked_machine_id(&text, &path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => create_machine_id(&path),
            Err(error) => {
                Err(error).with_context(|| format!("не удалось прочитать {}", path.display()))
            }
        }
    }

    fn subscription_dir(&self, name: &str) -> PathBuf {
        self.root.join("subscriptions").join(key(name))
    }

    /// Состояние подписки и её последний рабочий ответ. Повреждённое состояние
    /// заменяется пустым: подписка просто получится заново.
    pub(crate) fn load(&self, name: &str) -> (SubState, Option<Vec<u8>>) {
        let dir = self.subscription_dir(name);
        let state = fs::read(dir.join("state.json")).map_or_else(
            |_| SubState::default(),
            |bytes| {
                serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                    warn!("состояние подписки «{name}» повреждено и будет создано заново");
                    SubState::default()
                })
            },
        );
        let body = if state.fetched_at > 0 {
            fs::read(dir.join("body")).ok()
        } else {
            None
        };
        (state, body)
    }

    pub(crate) fn save_body(&self, name: &str, body: &[u8]) -> Result<()> {
        let dir = self.subscription_dir(name);
        create_private_dir(&dir)?;
        write_atomic(&dir.join("body"), body)
    }

    pub(crate) fn save_state(&self, name: &str, state: &SubState) -> Result<()> {
        let dir = self.subscription_dir(name);
        create_private_dir(&dir)?;
        let json =
            serde_json::to_vec_pretty(state).context("не удалось сериализовать состояние")?;
        write_atomic(&dir.join("state.json"), &json)
    }

    /// Закрепление, сделанное через API. `None` — файла нет (действует `selection.pin`
    /// из настроек), `Some(None)` — закрепление снято (автоматика, даже если в настройках
    /// есть `selection.pin`), `Some(Some(id))` — закреплён узел «подписка/имя».
    pub(crate) fn load_pin(&self) -> Option<Option<String>> {
        let bytes = fs::read(self.path(PIN_FILE)).ok()?;
        serde_json::from_slice::<PinFile>(&bytes)
            .inspect_err(|_| warn!("файл закрепления {PIN_FILE} повреждён и игнорируется"))
            .ok()
            .map(|file| file.node)
    }

    pub(crate) fn save_pin(&self, node: Option<&str>) -> Result<()> {
        let json = serde_json::to_vec(&PinFile {
            node: node.map(str::to_owned),
        })
        .context("не удалось сериализовать закрепление")?;
        self.write_file(PIN_FILE, &json)
    }
}

const PIN_FILE: &str = "pin.json";

#[derive(Serialize, Deserialize)]
struct PinFile {
    node: Option<String>,
}

fn create_machine_id(path: &Path) -> Result<String> {
    let id = random_hex()?;
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path);
    match created {
        Ok(mut file) => {
            writeln!(file, "{id}")
                .and_then(|()| file.sync_all())
                .with_context(|| format!("не удалось записать {}", path.display()))?;
            Ok(id)
        }
        // Одновременно запущенная команда успела раньше: идентификатор один на всех.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let text = fs::read_to_string(path)
                .with_context(|| format!("не удалось прочитать {}", path.display()))?;
            checked_machine_id(&text, path)
        }
        Err(error) => Err(error).with_context(|| format!("не удалось создать {}", path.display())),
    }
}

/// Имя каталога подписки: безопасные символы имени и отпечаток целого имени. Имя из
/// настроек не может увести запись за пределы каталога состояния.
fn key(name: &str) -> String {
    let readable: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(24)
        .collect();
    format!("{readable}-{:016x}", fnv1a(name.as_bytes()))
}

fn checked_machine_id(text: &str, path: &Path) -> Result<String> {
    let id = text.trim();
    if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(id.to_owned())
    } else {
        bail!(
            "файл {} повреждён: ожидается 32 шестнадцатеричных символа; исправьте или удалите его",
            path.display()
        )
    }
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .context("не удалось получить случайные байты из /dev/urandom")?;
    Ok(bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    }))
}

pub(crate) fn create_private_dir(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(path)
        .with_context(|| format!("не удалось создать каталог {}", path.display()))?;
    let mode = fs::metadata(path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o777 != DIR_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))
            .with_context(|| format!("не удалось ограничить права каталога {}", path.display()))?;
    }
    Ok(())
}

fn temp_path(path: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = path
        .parent()
        .with_context(|| format!("у {} нет каталога", path.display()))?;
    let name = path.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

fn write_temp(tmp: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(tmp)?;
    file.write_all(data)?;
    file.sync_all()
}

/// Файл появляется целиком или не меняется совсем; права 0600.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = temp_path(path)?;
    let result = write_temp(&tmp, data).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("не удалось записать {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn open_creates_private_directories() {
        let temp = TempDir::new("open");
        let root = temp.path().join("a/b/state");
        let store = Store::open(root.clone()).unwrap();
        assert_eq!(store.root(), root);
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("subscriptions")), 0o700);
    }

    #[test]
    fn open_tightens_an_existing_directory() {
        let temp = TempDir::new("tighten");
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        Store::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(mode(temp.path()), 0o700);
    }

    #[test]
    fn files_are_private_and_replaced_whole() {
        let temp = TempDir::new("atomic");
        let path = temp.path().join("file");
        write_atomic(&path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert_eq!(mode(&path), 0o600);
        write_atomic(&path, b"two two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two two");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(names(temp.path()), ["file"]);
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file() {
        let temp = TempDir::new("failed");
        let path = temp.path().join("target");
        fs::create_dir(&path).unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert!(path.is_dir());
        assert_eq!(names(temp.path()), ["target"]);
    }

    #[test]
    fn writing_into_a_missing_directory_fails_cleanly() {
        let temp = TempDir::new("missing");
        let path = temp.path().join("nope/file");
        assert!(write_atomic(&path, b"data").is_err());
        assert!(names(temp.path()).is_empty());
    }

    #[test]
    fn machine_id_is_created_once_and_private() {
        let temp = TempDir::new("machine");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        let first = store.machine_id().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(store.machine_id().unwrap(), first);
        assert_eq!(mode(&temp.path().join("machine-id")), 0o600);
        let reopened = Store::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(reopened.machine_id().unwrap(), first);
    }

    #[test]
    fn a_corrupt_machine_id_is_reported_not_replaced() {
        let temp = TempDir::new("corrupt-id");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        fs::write(temp.path().join("machine-id"), "not an id").unwrap();
        let error = store.machine_id().unwrap_err();
        assert!(error.to_string().contains("повреждён"));
        assert_eq!(
            fs::read_to_string(temp.path().join("machine-id")).unwrap(),
            "not an id"
        );
    }

    #[test]
    fn subscription_state_round_trips() {
        let temp = TempDir::new("roundtrip");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        let (empty, body) = store.load("основная");
        assert_eq!(empty, SubState::default());
        assert!(body.is_none());

        let state = SubState {
            source: "abc".to_owned(),
            status: 200,
            headers: vec![("Profile-Title".to_owned(), "Тест".to_owned())],
            fetched_at: 1_000,
            next_update: 2_000,
            last_error: Some("сбой".to_owned()),
            replaced_url: Some("https://new.example.com/sub/abcd".to_owned()),
            fallback_url: None,
        };
        store.save_body("основная", b"ss://body").unwrap();
        store.save_state("основная", &state).unwrap();
        let (loaded, body) = store.load("основная");
        assert_eq!(loaded, state);
        assert_eq!(body.as_deref(), Some(b"ss://body".as_slice()));

        let dir = temp.path().join("subscriptions").join(key("основная"));
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("body")), 0o600);
        assert_eq!(mode(&dir.join("state.json")), 0o600);
    }

    #[test]
    fn a_body_without_a_fetch_time_is_not_a_cache() {
        let temp = TempDir::new("nocache");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        store.save_body("s", b"data").unwrap();
        store.save_state("s", &SubState::default()).unwrap();
        assert!(store.load("s").1.is_none());
    }

    #[test]
    fn a_corrupt_state_file_becomes_an_empty_state() {
        let temp = TempDir::new("corrupt-state");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        store.save_state("s", &SubState::default()).unwrap();
        let path = temp
            .path()
            .join("subscriptions")
            .join(key("s"))
            .join("state.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(store.load("s").0, SubState::default());
    }

    #[test]
    fn the_pin_survives_a_reopen_and_can_be_switched_off() {
        let temp = TempDir::new("pin");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(store.load_pin(), None);

        store.save_pin(Some("основная/NL-1")).unwrap();
        assert_eq!(mode(&temp.path().join("pin.json")), 0o600);
        let reopened = Store::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(reopened.load_pin(), Some(Some("основная/NL-1".to_owned())));

        store.save_pin(None).unwrap();
        assert_eq!(store.load_pin(), Some(None));
    }

    #[test]
    fn a_corrupt_pin_file_is_ignored() {
        let temp = TempDir::new("pin-corrupt");
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        fs::write(temp.path().join("pin.json"), "{ not json").unwrap();
        assert_eq!(store.load_pin(), None);
    }

    #[test]
    fn keys_are_safe_and_distinct() {
        for name in ["..", "../../etc", "a/b", "имя", "with space", ""] {
            let key = key(name);
            assert!(
                !key.contains('/') && !key.contains('.') && !key.contains(' '),
                "{key}"
            );
            assert!(key.len() <= 24 + 17, "{key}");
        }
        assert_eq!(key("основная"), key("основная"));
        assert_ne!(key("а"), key("б"));
        assert_ne!(key("a"), key("A"));
    }
}
