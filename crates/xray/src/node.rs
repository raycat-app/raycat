use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Узел подписки в виде outbound'ов xray.
///
/// `outbounds[0]` — точка выхода, остальные — её цепочка: на них ссылаются
/// `streamSettings.sockopt.dialerProxy` или `proxySettings.tag`. Теги внутри узла
/// уникальны; при сборке конфига компилятор переименовывает их.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub outbounds: Vec<Value>,
}
