use crate::error::Problems;
use crate::load::Env;
use crate::raw::{Raw, RawSubscription};
use crate::validate::{
    APP_HINT, BOOL_HINT, LEVEL_HINT, LISTEN_HINT, MODE_HINT, PLATFORM_HINT, parse_app, parse_bool,
    parse_level, parse_listen, parse_mode_kind, parse_platform,
};

pub(crate) const CONFIG: &str = "RAYCAT_CONFIG";
const SUBSCRIPTION: &str = "RAYCAT_SUBSCRIPTION";
const APP: &str = "RAYCAT_APP";
const PLATFORM: &str = "RAYCAT_PLATFORM";
const SEED: &str = "RAYCAT_SEED";
const MODE: &str = "RAYCAT_MODE";
const LISTEN: &str = "RAYCAT_LISTEN";
const KILL_SWITCH: &str = "RAYCAT_KILL_SWITCH";
const LOG: &str = "RAYCAT_LOG";

const ENV_SUBSCRIPTION_NAME: &str = "основная";

/// Пустая переменная считается незаданной: `${VAR:-}` в compose не должен
/// молча выключать, например, kill switch.
pub(crate) fn value<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

/// Переменные окружения перекрывают файл. `RAYCAT_SUBSCRIPTION`, `RAYCAT_APP`
/// и `RAYCAT_PLATFORM` относятся к первой (основной) подписке; если подписок
/// в файле нет, она создаётся.
pub(crate) fn apply(raw: &mut Raw, env: &Env, p: &mut Problems) {
    let get = |key: &str| value(env, key);
    let (url, app, platform) = (get(SUBSCRIPTION), get(APP), get(PLATFORM));
    if url.is_some() || app.is_some() || platform.is_some() {
        if raw.subscription.is_empty() {
            raw.subscription.push(RawSubscription {
                name: Some(ENV_SUBSCRIPTION_NAME.to_owned()),
                ..RawSubscription::default()
            });
        }
        if let Some(first) = raw.subscription.first_mut() {
            if let Some(url) = url {
                first.url = Some(url.to_owned());
            }
            if let Some(app) = app {
                set_text(&mut first.app, APP, app, parse_app, APP_HINT, p);
            }
            if let Some(platform) = platform {
                set_text(
                    &mut first.platform,
                    PLATFORM,
                    platform,
                    parse_platform,
                    PLATFORM_HINT,
                    p,
                );
            }
        }
    }
    if let Some(seed) = get(SEED) {
        raw.device.seed = Some(seed.to_owned());
        raw.device.machine_id = None;
    }
    if let Some(mode) = get(MODE) {
        set_text(
            &mut raw.mode.kind,
            MODE,
            mode,
            parse_mode_kind,
            MODE_HINT,
            p,
        );
    }
    if let Some(listen) = get(LISTEN) {
        set_text(
            &mut raw.mode.listen,
            LISTEN,
            listen,
            parse_listen,
            LISTEN_HINT,
            p,
        );
    }
    if let Some(flag) = get(KILL_SWITCH) {
        match parse_bool(flag) {
            Some(flag) => raw.mode.kill_switch = Some(flag),
            None => p.add(KILL_SWITCH, BOOL_HINT),
        }
    }
    if let Some(level) = get(LOG) {
        set_text(&mut raw.log.level, LOG, level, parse_level, LEVEL_HINT, p);
    }
}

fn set_text<T>(
    slot: &mut Option<String>,
    key: &str,
    value: &str,
    parse: fn(&str) -> Option<T>,
    hint: &str,
    p: &mut Problems,
) {
    if parse(value).is_some() {
        *slot = Some(value.to_owned());
    } else {
        p.add(key, hint);
    }
}
