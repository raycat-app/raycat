//! Чтение ответа HTTP/1.1: строка статуса, заголовки, способ передачи тела
//! (`Content-Length`, chunked с трейлерами, до закрытия соединения) и распаковка.
//!
//! Сервер недоверенный: каждая строка, блок заголовков, чанк, трейлеры и распакованное
//! тело ограничены по размеру.

use std::io::{self, BufRead, BufReader, Read};
use std::net::IpAddr;

use anyhow::{Context, Result, anyhow, bail};

pub(crate) const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_TRAILERS: usize = 64;
const MAX_INTERIM_RESPONSES: usize = 8;

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    /// Заголовки в порядке получения, с исходным регистром.
    pub headers: Vec<(String, String)>,
    /// Тело с уже снятым `Content-Encoding`.
    pub body: Vec<u8>,
    /// Адрес ответившего сервера; только при прямом подключении, через прокси
    /// `None`.
    pub peer: Option<IpAddr>,
}

impl Response {
    /// Первый заголовок с таким именем (без учёта регистра).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Заменяет управляющие символы в тексте от сервера (escape-последовательности
/// терминала, поддельные строки лога), прежде чем он попадёт в сообщение или лог.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Читает строку (вместе с терминатором) не длиннее `limit` байт; на конце потока
/// возвращает пустую.
fn read_line(reader: &mut impl BufRead, limit: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() > limit {
        bail!("строка ответа длиннее {limit} байт");
    }
    Ok(line)
}

fn trim_line(line: &[u8]) -> String {
    String::from_utf8_lossy(line)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

struct Head {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
}

fn read_head(reader: &mut impl BufRead) -> Result<Head> {
    let mut head_bytes = 0usize;
    let mut next_line = |reader: &mut _| -> Result<String> {
        let line = read_line(reader, MAX_LINE_BYTES)?;
        if line.is_empty() {
            bail!("соединение закрыто до конца заголовков ответа");
        }
        head_bytes += line.len();
        if head_bytes > MAX_HEAD_BYTES {
            bail!("заголовки ответа длиннее {MAX_HEAD_BYTES} байт");
        }
        Ok(trim_line(&line))
    };

    let status_line = next_line(reader)?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status = parts.next().and_then(|s| s.parse::<u16>().ok());
    let (true, Some(status @ 100..=999)) = (version.starts_with("HTTP/1."), status) else {
        bail!("ответ не похож на HTTP/1.x");
    };
    let reason = sanitize(parts.next().unwrap_or_default());

    let mut headers = Vec::new();
    loop {
        let line = next_line(reader)?;
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok(Head {
        status,
        reason,
        headers,
    })
}

pub(crate) fn read_response(
    stream: &mut impl Read,
    method: &str,
    max_body: usize,
) -> Result<Response> {
    let mut reader = BufReader::new(stream);
    let mut interim = 0;
    let head = loop {
        let head = read_head(&mut reader)?;
        // 1xx, кроме 101, предшествуют настоящему ответу (RFC 9110, п. 15.2).
        if (100..200).contains(&head.status) && head.status != 101 {
            interim += 1;
            if interim > MAX_INTERIM_RESPONSES {
                bail!("слишком много промежуточных ответов (1xx)");
            }
            continue;
        }
        break head;
    };
    let header = |name: &str| {
        head.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };

    let no_body = method.eq_ignore_ascii_case("HEAD")
        || head.status == 101
        || head.status == 204
        || head.status == 304;
    let body = if no_body {
        Vec::new()
    } else if header("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        read_chunked(&mut reader, max_body)?
    } else if let Some(len) = header("content-length") {
        let len: usize = len
            .parse()
            .map_err(|_| anyhow!("неверный Content-Length"))?;
        if len > max_body {
            bail!("тело ответа ({len} байт) больше лимита в {max_body} байт");
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).context("чтение тела ответа")?;
        body
    } else {
        read_to_close(&mut reader, max_body)?
    };

    let body = match header("content-encoding") {
        Some(encoding) if !body.is_empty() => decode(body, encoding, max_body)?,
        _ => body,
    };
    Ok(Response {
        status: head.status,
        reason: head.reason,
        headers: head.headers,
        body,
        peer: None,
    })
}

fn read_chunked(reader: &mut impl BufRead, max_body: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader, MAX_LINE_BYTES)?;
        if line.is_empty() {
            bail!("соединение закрыто посреди chunked-тела");
        }
        let line = trim_line(&line);
        let size = line.split(';').next().unwrap_or_default().trim();
        if size.is_empty() || size.len() > 16 || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("неверный размер чанка");
        }
        let size = u64::from_str_radix(size, 16)?;
        if size == 0 {
            // Секция трейлеров заканчивается пустой строкой (или концом потока у
            // неряшливых серверов).
            for _ in 0..=MAX_TRAILERS {
                let line = read_line(reader, MAX_LINE_BYTES)?;
                if line.is_empty() || trim_line(&line).is_empty() {
                    return Ok(body);
                }
            }
            bail!("больше {MAX_TRAILERS} полей трейлера");
        }
        let end = usize::try_from(size)
            .ok()
            .and_then(|size| body.len().checked_add(size))
            .filter(|&end| end <= max_body)
            .ok_or_else(|| anyhow!("тело ответа больше лимита в {max_body} байт"))?;
        let start = body.len();
        body.resize(end, 0);
        reader
            .read_exact(&mut body[start..])
            .context("чтение чанка")?;
        read_line(reader, MAX_LINE_BYTES)?;
    }
}

fn read_to_close(reader: &mut impl Read, max_body: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(body),
            Ok(n) => {
                if body.len() + n > max_body {
                    bail!("тело ответа больше лимита в {max_body} байт");
                }
                body.extend_from_slice(&buf[..n]);
            }
            // Многие серверы закрывают TLS без `close_notify`, тело при этом полное.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(body),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e).context("чтение тела ответа"),
        }
    }
}

/// Снимает `Content-Encoding`: кодировки перечислены в порядке применения, поэтому
/// снимаются в обратном.
fn decode(mut body: Vec<u8>, encodings: &str, max_body: usize) -> Result<Vec<u8>> {
    for encoding in encodings.rsplit(',').map(|e| e.trim().to_ascii_lowercase()) {
        body = match encoding.as_str() {
            "" | "identity" => body,
            "gzip" | "x-gzip" => inflate(flate2::read::MultiGzDecoder::new(&body[..]), max_body)?,
            "deflate" => {
                // По RFC это zlib, но часть серверов шлёт «сырой» deflate.
                inflate(flate2::read::ZlibDecoder::new(&body[..]), max_body)
                    .or_else(|_| inflate(flate2::read::DeflateDecoder::new(&body[..]), max_body))?
            }
            "br" => inflate(
                brotli_decompressor::Decompressor::new(&body[..], 4096),
                max_body,
            )?,
            "zstd" => inflate(
                ruzstd::decoding::StreamingDecoder::new(&body[..])
                    .map_err(|e| anyhow!("zstd: {e}"))?,
                max_body,
            )?,
            _ => bail!("неподдерживаемый Content-Encoding"),
        };
    }
    Ok(body)
}

/// Дочитывает распаковщик до конца, отказываясь от вывода больше `max` байт.
fn inflate(decoder: impl Read, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    decoder
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .context("распаковка")?;
    if out.len() > max {
        bail!("распакованные данные больше лимита в {max} байт");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

    use super::*;

    fn parse(raw: &[u8]) -> Response {
        read_response(&mut Cursor::new(raw.to_vec()), "GET", 1 << 20).unwrap()
    }

    fn parse_err(raw: &[u8], max: usize) -> String {
        format!(
            "{:#}",
            read_response(&mut Cursor::new(raw.to_vec()), "GET", max).unwrap_err()
        )
    }

    fn compress(mut encoder: impl Write, data: &[u8]) {
        encoder.write_all(data).unwrap();
        encoder.flush().unwrap();
    }

    #[test]
    fn reads_content_length_body() {
        let r = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhelloEXTRA");
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello");
        assert_eq!(r.header("x-a"), Some("b"));
    }

    #[test]
    fn reads_chunked_body() {
        let r = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext\r\nhello\r\n6\r\n world\r\n0\r\nX-T: 1\r\n\r\n");
        assert_eq!(r.body, b"hello world");
    }

    #[test]
    fn rejects_hostile_chunks() {
        let chunked =
            |rest: &str| format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{rest}");
        // Без checked-арифметики `len + size` переполнился бы.
        let e = parse_err(chunked("1\r\na\r\nffffffffffffffff\r\n").as_bytes(), 100);
        assert!(e.contains("лимита"), "{e}");
        assert!(
            parse_err(chunked("10000000000000000\r\n").as_bytes(), 100).contains("размер чанка")
        );
        assert!(parse_err(chunked("-1\r\n").as_bytes(), 100).contains("размер чанка"));
        assert!(parse_err(chunked("5\r\nhel").as_bytes(), 100).contains("чтение чанка"));
        let trailers = "X: 1\r\n".repeat(MAX_TRAILERS + 1);
        assert!(
            parse_err(chunked(&format!("0\r\n{trailers}\r\n")).as_bytes(), 100)
                .contains("трейлера")
        );
    }

    #[test]
    fn bounds_lines() {
        let long = format!(
            "HTTP/1.1 200 OK\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_LINE_BYTES)
        );
        assert!(parse_err(long.as_bytes(), 100).contains("строка ответа"));
        let many = format!(
            "HTTP/1.1 200 OK\r\n{}\r\n",
            "X: aaaaaaaaaaaaaaaa\r\n".repeat(4000)
        );
        assert!(parse_err(many.as_bytes(), 100).contains("заголовки ответа"));
    }

    #[test]
    fn skips_interim_responses() {
        let r = parse(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: x\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        assert_eq!((r.status, &r.body[..]), (200, &b"ok"[..]));
        let flood = "HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_INTERIM_RESPONSES + 1);
        assert!(parse_err(flood.as_bytes(), 100).contains("промежуточных"));
    }

    #[test]
    fn reads_until_close_and_head() {
        let r = parse(b"HTTP/1.0 404 Not Found\r\n\r\nnope");
        assert_eq!(
            (r.status, r.reason.as_str(), &r.body[..]),
            (404, "Not Found", &b"nope"[..])
        );
        let r = read_response(
            &mut Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n".to_vec()),
            "HEAD",
            100,
        )
        .unwrap();
        assert!(r.body.is_empty());
        let r = parse(b"HTTP/1.1 204 No Content\r\nContent-Encoding: gzip\r\n\r\n");
        assert!(r.body.is_empty());
    }

    #[test]
    fn keeps_header_order_and_case() {
        let r = parse(b"HTTP/1.1 200 OK\r\nX-Zed: 1\r\nAccept-Ranges: none\r\nx-Low: 2\r\nContent-Length: 0\r\n\r\n");
        let names: Vec<&str> = r.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["X-Zed", "Accept-Ranges", "x-Low", "Content-Length"]);
    }

    #[test]
    fn reason_phrase_is_sanitized() {
        let r = parse(b"HTTP/1.1 200 O\x1b[31mK\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(r.reason, "O [31mK");
    }

    #[test]
    fn empty_encoded_body_is_not_decoded() {
        let r = parse(b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nContent-Length: 0\r\n\r\n");
        assert!(r.body.is_empty());
    }

    #[test]
    fn decodes_gzip() {
        let mut gz = Vec::new();
        compress(
            GzEncoder::new(&mut gz, Compression::default()),
            b"proxies: []",
        );
        let mut raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            gz.len()
        )
        .into_bytes();
        raw.extend_from_slice(&gz);
        assert_eq!(parse(&raw).body, b"proxies: []");
    }

    #[test]
    fn decodes_deflate_zlib_and_raw() {
        let mut zlib = Vec::new();
        compress(
            ZlibEncoder::new(&mut zlib, Compression::default()),
            b"proxies: []",
        );
        assert_eq!(decode(zlib, "deflate", 1024).unwrap(), b"proxies: []");
        let mut raw = Vec::new();
        compress(
            DeflateEncoder::new(&mut raw, Compression::default()),
            b"proxies: []",
        );
        assert_eq!(decode(raw, "deflate", 1024).unwrap(), b"proxies: []");
    }

    #[test]
    fn decodes_brotli() {
        // Несжатый мета-блок с «hello» и завершающий пустой мета-блок.
        let mut packed = vec![0x40, 0x00, 0x10];
        packed.extend_from_slice(b"hello");
        packed.push(0x03);
        assert_eq!(decode(packed, "br", 1024).unwrap(), b"hello");
    }

    #[test]
    fn decodes_zstd() {
        let packed = ruzstd::encoding::compress_to_vec(
            &b"proxies: []"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        assert_eq!(decode(packed, "zstd", 1024).unwrap(), b"proxies: []");
    }

    #[test]
    fn undoes_stacked_encodings_in_reverse() {
        let mut gz = Vec::new();
        compress(GzEncoder::new(&mut gz, Compression::default()), b"data");
        let mut zlib = Vec::new();
        compress(ZlibEncoder::new(&mut zlib, Compression::default()), &gz);
        assert_eq!(decode(zlib, "gzip, deflate", 1024).unwrap(), b"data");
        assert!(decode(b"x".to_vec(), "rot13", 1024).is_err());
    }

    #[test]
    fn decompression_is_bounded() {
        let mut gz = Vec::new();
        compress(
            GzEncoder::new(&mut gz, Compression::best()),
            &vec![0u8; 1 << 20],
        );
        assert!(gz.len() < 4096);
        let e = format!("{:#}", decode(gz, "gzip", 1000).unwrap_err());
        assert!(e.contains("лимита"), "{e}");
    }

    #[test]
    fn enforces_body_limit() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n".to_vec();
        assert!(read_response(&mut Cursor::new(raw), "GET", 10).is_err());
        let raw = b"HTTP/1.0 200 OK\r\n\r\n0123456789ABCDEF".to_vec();
        assert!(read_response(&mut Cursor::new(raw), "GET", 10).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_response(&mut Cursor::new(b"SSH-2.0\r\n\r\n".to_vec()), "GET", 10).is_err());
        assert!(
            read_response(
                &mut Cursor::new(b"HTTP/1.1 20 OK\r\n\r\n".to_vec()),
                "GET",
                10
            )
            .is_err()
        );
        assert!(read_response(&mut Cursor::new(Vec::new()), "GET", 10).is_err());
    }
}
