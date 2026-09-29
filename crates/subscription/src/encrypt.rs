//! Расшифровка тела ответа с заголовком `Encrypt-Tag`.
//!
//! Формат (по открытым источникам, проверен только на данных из тестов):
//! шифр AES-128-GCM с фиксированным 12-байтовым nonce из ASCII `k`; тело ответа —
//! шифртекст в base64 (без тега), заголовок `Encrypt-Tag` — 16-байтовый тег GCM в
//! base64. Ключ в ответе не передаётся: панель выбирает его из таблицы ключей по
//! параметру `key` ссылки подписки. Таблицу ключей крейт не содержит, ключ задаёт
//! вызывающий код.

use std::fmt;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};

use crate::text::decode_base64;

const NONCE: [u8; 12] = *b"kkkkkkkkkkkk";
const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptError {
    /// Тело не является текстом в base64.
    Body,
    /// `Encrypt-Tag` не base64 из 16 байт.
    Tag,
    /// Тег не сошёлся: ключ не тот или данные повреждены.
    Verify,
}

impl fmt::Display for DecryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Body => "тело зашифрованного ответа не в формате base64",
            Self::Tag => "заголовок Encrypt-Tag не содержит 16 байт в base64",
            Self::Verify => "расшифровка не удалась: неверный ключ или повреждённые данные",
        })
    }
}

impl std::error::Error for DecryptError {}

/// Расшифровывает тело ответа по заголовку `Encrypt-Tag` и 16-байтовому ключу.
pub fn decrypt_body(body: &[u8], tag: &str, key: &[u8; 16]) -> Result<Vec<u8>, DecryptError> {
    let text = std::str::from_utf8(body).map_err(|_| DecryptError::Body)?;
    let mut data = decode_base64(text).ok_or(DecryptError::Body)?;
    let tag = decode_base64(tag.trim())
        .filter(|bytes| bytes.len() == TAG_LEN)
        .ok_or(DecryptError::Tag)?;
    // aead ждёт тег сразу после шифртекста.
    data.extend_from_slice(&tag);
    let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| DecryptError::Verify)?;
    cipher
        .decrypt(&Nonce::<U12>::from(NONCE), data.as_slice())
        .map_err(|_| DecryptError::Verify)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

    use super::*;

    const KEY: [u8; 16] = *b"0123456789abcdef";

    fn encrypt(plain: &[u8], key: &[u8; 16]) -> (String, String) {
        let cipher = Aes128Gcm::new_from_slice(key).unwrap();
        let sealed = cipher
            .encrypt(&Nonce::<U12>::from(NONCE), plain)
            .unwrap();
        let (data, tag) = sealed.split_at(sealed.len() - TAG_LEN);
        (STANDARD.encode(data), STANDARD.encode(tag))
    }

    #[test]
    fn round_trip() {
        let plain = "vless://00000000-0000-0000-0000-000000000000@a.example.com:443#Узел".as_bytes();
        let (body, tag) = encrypt(plain, &KEY);
        assert_eq!(decrypt_body(body.as_bytes(), &tag, &KEY).unwrap(), plain);
    }

    #[test]
    fn base64_variants_of_body_and_tag() {
        let plain = b"[{\"remarks\": \"x\"}]";
        let (body, tag) = encrypt(plain, &KEY);
        let raw_body = STANDARD.decode(&body).unwrap();
        let raw_tag = STANDARD.decode(&tag).unwrap();
        let url_body = URL_SAFE_NO_PAD.encode(&raw_body);
        let url_tag = URL_SAFE_NO_PAD.encode(&raw_tag);
        assert_eq!(decrypt_body(url_body.as_bytes(), &url_tag, &KEY).unwrap(), plain);
        let wrapped = format!("{}\r\n{}\r\n", &body[..4], &body[4..]);
        assert_eq!(decrypt_body(wrapped.as_bytes(), &format!(" {tag} "), &KEY).unwrap(), plain);
    }

    #[test]
    fn failures() {
        let (body, tag) = encrypt(b"secret", &KEY);
        let mut wrong_key = KEY;
        wrong_key[0] ^= 1;
        assert_eq!(decrypt_body(body.as_bytes(), &tag, &wrong_key), Err(DecryptError::Verify));

        let mut damaged = STANDARD.decode(&body).unwrap();
        damaged[0] ^= 1;
        let damaged = STANDARD.encode(damaged);
        assert_eq!(decrypt_body(damaged.as_bytes(), &tag, &KEY), Err(DecryptError::Verify));

        assert_eq!(decrypt_body(b"!!!", &tag, &KEY), Err(DecryptError::Body));
        assert_eq!(decrypt_body(&[0xff, 0xfe], &tag, &KEY), Err(DecryptError::Body));
        assert_eq!(decrypt_body(b"", &tag, &KEY), Err(DecryptError::Body));
        assert_eq!(decrypt_body(body.as_bytes(), "AAAA", &KEY), Err(DecryptError::Tag));
        assert_eq!(decrypt_body(body.as_bytes(), "", &KEY), Err(DecryptError::Tag));
    }
}
