//! Клиент: подключение (TCP, CONNECT через прокси, TLS) и отправка запроса.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::response::{self, Response};
use crate::url::{Scheme, Url};
use crate::{dns, socket};

pub struct Request<'a> {
    pub method: &'a str,
    /// Цель запроса, например `/sub/abc?x=1`.
    pub target: &'a str,
    /// Полный упорядоченный список заголовков, пишется как есть.
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct Client {
    pub connect_timeout: Duration,
    /// Самая долгая тишина между двумя чтениями или записями.
    pub io_timeout: Duration,
    /// Предел по времени на весь обмен, включая подключение.
    pub total_timeout: Duration,
    /// Прокси `http://host:port`, через который идёт `CONNECT`.
    pub proxy: Option<Url>,
    /// Значение заголовка `Proxy-Authorization` для `CONNECT`, например `Basic …`.
    pub proxy_authorization: Option<String>,
    pub max_body: usize,
    /// `SO_MARK` для исходящих TCP-соединений и DNS-запросов, чтобы kill switch шлюза
    /// их пропускал. Нужен `CAP_NET_ADMIN`; без него метка не ставится.
    pub mark: Option<u32>,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            io_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(90),
            proxy: None,
            proxy_authorization: None,
            max_body: 32 * 1024 * 1024,
            mark: None,
        }
    }
}

impl Client {
    /// Отправляет запрос на сервер из `url` (схема, хост и порт берутся оттуда,
    /// цель и заголовки — из `req`) и читает ответ целиком.
    pub fn send(&self, url: &Url, req: &Request<'_>) -> Result<Response> {
        check_request(req)?;
        let deadline = Instant::now() + self.total_timeout;
        let (inner, peer) = self.connect(url, deadline)?;
        let mut stream = Stream {
            inner,
            deadline,
            io_timeout: self.io_timeout,
        };
        write_request(&mut stream, req)?;
        let mut response = response::read_response(&mut stream, req.method, self.max_body)?;
        response.peer = peer;
        Ok(response)
    }

    /// Как [`Client::send`], но тело не копится в памяти: `sink` получает куски по мере
    /// чтения и возвращает `false`, чтобы прервать загрузку. В `Response::body` тела нет.
    pub fn stream(
        &self,
        url: &Url,
        req: &Request<'_>,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Response> {
        check_request(req)?;
        let deadline = Instant::now() + self.total_timeout;
        let (inner, peer) = self.connect(url, deadline)?;
        let mut stream = Stream {
            inner,
            deadline,
            io_timeout: self.io_timeout,
        };
        write_request(&mut stream, req)?;
        let mut response = response::stream_response(&mut stream, req.method, sink)?;
        response.peer = peer;
        Ok(response)
    }

    fn connect(&self, url: &Url, deadline: Instant) -> Result<(Inner, Option<IpAddr>)> {
        let (tcp, peer) = if let Some(proxy) = &self.proxy {
            (self.tunnel(proxy, &url.host, url.port, deadline)?, None)
        } else {
            let tcp = self.dial(&url.host, url.port, deadline)?;
            let peer = tcp.peer_addr().ok().map(|a| a.ip());
            (tcp, peer)
        };
        if url.scheme == Scheme::Http {
            return Ok((Inner::Tcp(tcp), peer));
        }
        let name = rustls::pki_types::ServerName::try_from(url.host.clone())
            .context("недопустимое имя сервера для TLS")?;
        let conn = rustls::ClientConnection::new(tls_config()?, name)?;
        Ok((
            Inner::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))),
            peer,
        ))
    }

    fn dial(&self, host: &str, port: u16, deadline: Instant) -> Result<TcpStream> {
        let addrs: Vec<SocketAddr> = match self.mark {
            // Kill switch не выпускает обычный DNS: спрашиваем в обход (см. dns.rs).
            Some(mark) if dns::needs_marked_lookup(host) => {
                dns::resolve(host, port, mark, deadline)?
            }
            _ => (host, port)
                .to_socket_addrs()
                .with_context(|| format!("разрешение имени {host}"))?
                .collect(),
        };
        let mut last_err = None;
        for addr in addrs {
            let timeout = self.connect_timeout.min(remaining(deadline)?);
            let connected = match self.mark {
                Some(mark) => socket::connect_marked(&addr, timeout, mark),
                None => TcpStream::connect_timeout(&addr, timeout),
            };
            match connected {
                Ok(tcp) => {
                    tcp.set_nodelay(true)?;
                    return Ok(tcp);
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(match last_err {
            Some(e) => anyhow!(e).context(format!("подключение к {host}:{port}")),
            None => anyhow!("у {host} нет адресов"),
        })
    }

    fn tunnel(&self, proxy: &Url, host: &str, port: u16, deadline: Instant) -> Result<TcpStream> {
        if proxy.scheme != Scheme::Http {
            bail!("прокси по https не поддерживается, нужен http://host:port");
        }
        let tcp = self.dial(&proxy.host, proxy.port, deadline)?;
        let mut stream = Stream {
            inner: Inner::Tcp(tcp),
            deadline,
            io_timeout: self.io_timeout,
        };
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let auth = match &self.proxy_authorization {
            Some(value) if value.chars().any(char::is_control) => {
                bail!("недопустимый символ в Proxy-Authorization");
            }
            Some(value) => format!("Proxy-Authorization: {value}\r\n"),
            None => String::new(),
        };
        write!(
            stream,
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n"
        )
        .context("отправка CONNECT")?;
        // Читаем побайтно: за концом ответа прокси нельзя потребить ни байта,
        // дальше идёт уже трафик туннеля.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > response::MAX_HEAD_BYTES || stream.read(&mut byte)? == 0 {
                bail!("прокси закрыл соединение во время CONNECT");
            }
            head.push(byte[0]);
        }
        let status_line = String::from_utf8_lossy(&head);
        let status = status_line.split_whitespace().nth(1).unwrap_or_default();
        if status != "200" {
            bail!(
                "прокси отклонил CONNECT: {}",
                response::sanitize(status_line.lines().next().unwrap_or_default())
            );
        }
        match stream.inner {
            Inner::Tcp(tcp) => Ok(tcp),
            Inner::Tls(_) => bail!("внутренняя ошибка: туннель к прокси построен поверх TLS"),
        }
    }
}

fn write_request(stream: &mut Stream, req: &Request<'_>) -> Result<()> {
    let mut head = format!("{} {} HTTP/1.1\r\n", req.method, req.target);
    for (name, value) in req.headers {
        head.extend([name.as_str(), ": ", value.as_str(), "\r\n"]);
    }
    let has_length = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    if !req.body.is_empty() && !has_length {
        let len = req.body.len().to_string();
        head.extend(["Content-Length: ", len.as_str(), "\r\n"]);
    }
    head.push_str("\r\n");
    let mut wire = head.into_bytes();
    wire.extend_from_slice(req.body);
    stream.write_all(&wire).context("отправка запроса")?;
    stream.flush()?;
    Ok(())
}

/// Запрос уходит на провод как есть, поэтому управляющие символы в полях, которые
/// вызывающий код мог получить от недоверенного источника, дали бы подмену
/// заголовков или второй запрос.
fn check_request(req: &Request<'_>) -> Result<()> {
    let unfit = |c: char| c.is_whitespace() || c.is_control();
    if req.method.is_empty() || req.method.contains(unfit) {
        bail!("недопустимый символ в методе запроса");
    }
    if req.target.contains(unfit) {
        bail!("недопустимый символ в цели запроса");
    }
    for (name, value) in req.headers {
        if name.is_empty() || name.contains(|c: char| c == ':' || unfit(c)) {
            bail!("недопустимый символ в имени заголовка запроса");
        }
        if value.contains(|c: char| c != '\t' && c.is_control()) {
            bail!("недопустимый символ в значении заголовка запроса");
        }
    }
    Ok(())
}

/// Сколько осталось до `deadline`; ошибка, если срок уже вышел.
pub(crate) fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "истёк общий срок запроса",
        ));
    }
    Ok(left)
}

enum Inner {
    Tcp(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

/// Соединение, у сокета которого тайм-ауты выставляются заново перед каждым чтением
/// и записью: операция ждёт не дольше `min(io_timeout, время до deadline)`.
struct Stream {
    inner: Inner,
    deadline: Instant,
    io_timeout: Duration,
}

impl Stream {
    fn arm(&self) -> io::Result<()> {
        let timeout = Some(self.io_timeout.min(remaining(self.deadline)?));
        match &self.inner {
            Inner::Tcp(s) => {
                s.set_read_timeout(timeout)?;
                s.set_write_timeout(timeout)
            }
            Inner::Tls(s) => {
                s.sock.set_read_timeout(timeout)?;
                s.sock.set_write_timeout(timeout)
            }
        }
    }

    /// Тайм-аут сокета приходит как `EAGAIN`. Сокет ждёт меньшее из тайм-аута
    /// бездействия и общего срока, поэтому называем, что именно истекло.
    fn timed_out(&self, e: io::Error) -> io::Error {
        if !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            return e;
        }
        // Тайм-ауты сокета усекаются до микросекунд: допускаем чуть раннее пробуждение.
        let slack = Duration::from_millis(1);
        let deadline = self.deadline.checked_sub(slack).unwrap_or(self.deadline);
        if let Err(exceeded) = remaining(deadline) {
            return exceeded;
        }
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("сервер не отвечает уже {:?}", self.io_timeout),
        )
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.arm()?;
        let result = match &mut self.inner {
            Inner::Tcp(s) => s.read(buf),
            Inner::Tls(s) => s.read(buf),
        };
        result.map_err(|e| self.timed_out(e))
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.arm()?;
        let result = match &mut self.inner {
            Inner::Tcp(s) => s.write(buf),
            Inner::Tls(s) => s.write(buf),
        };
        result.map_err(|e| self.timed_out(e))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.arm()?;
        let result = match &mut self.inner {
            Inner::Tcp(s) => s.flush(),
            Inner::Tls(s) => s.flush(),
        };
        result.map_err(|e| self.timed_out(e))
    }
}

/// Общая конфигурация TLS: корни webpki-roots, провайдер ring (без aws-lc и OpenSSL).
/// ALPN не задаётся: клиент говорит только по HTTP/1.1, и расширение с `h2` ему ни к
/// чему.
fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    // Дополнительные корни для сетей с перехватом TLS (та же переменная, что у OpenSSL).
    if let Some(path) = std::env::var_os("SSL_CERT_FILE") {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        for cert in CertificateDer::pem_file_iter(&path)
            .with_context(|| format!("чтение SSL_CERT_FILE {}", Path::new(&path).display()))?
        {
            roots.add(cert?)?;
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| Arc::new(config)).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пара соединённых сокетов для тестов, которым нужен только поток для чтения
    /// и записи.
    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn total_deadline_stops_a_trickling_server() {
        let (client, mut server) = tcp_pair();
        let writer = std::thread::spawn(move || {
            let _ = server.write_all(b"HTTP/1.1 200 OK\r\n");
            for _ in 0..100 {
                if server.write_all(b"X: y\r\n").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let mut stream = Stream {
            inner: Inner::Tcp(client),
            deadline: Instant::now() + Duration::from_millis(200),
            io_timeout: Duration::from_secs(5),
        };
        let started = Instant::now();
        assert!(response::read_response(&mut stream, "GET", 1024).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(stream);
        writer.join().unwrap();
    }

    #[test]
    fn timeouts_name_what_ran_out() {
        let read = |deadline, io_timeout| {
            let (client, _server) = tcp_pair();
            let mut stream = Stream {
                inner: Inner::Tcp(client),
                deadline: Instant::now() + deadline,
                io_timeout,
            };
            stream.read(&mut [0; 8]).unwrap_err().to_string()
        };
        let short = Duration::from_millis(100);
        let long = Duration::from_secs(5);
        assert_eq!(read(short, long), "истёк общий срок запроса");
        assert_eq!(read(long, short), "сервер не отвечает уже 100ms");
    }

    #[test]
    fn tls_is_rustls_without_alpn() {
        let config = tls_config().unwrap();
        assert_eq!(config.alpn_protocols, Vec::<Vec<u8>>::new());
    }

    #[test]
    fn request_fields_are_checked() {
        let header = |name: &str, value: &str| vec![(name.to_owned(), value.to_owned())];
        let check = |method: &str, target: &str, headers: &[(String, String)]| {
            check_request(&Request {
                method,
                target,
                headers,
                body: &[],
            })
            .is_ok()
        };
        assert!(check("GET", "/a?b=c", &header("X-A", "v\tw")));
        assert!(!check("GE T", "/", &[]));
        assert!(!check("", "/", &[]));
        assert!(!check("GET", "/ HTTP/1.1\r\nX: 1", &[]));
        assert!(!check("GET", "/", &header("X-A", "v\r\nX-B: w")));
        assert!(!check("GET", "/", &header("X-A\nX-B", "v")));
        assert!(!check("GET", "/", &header("X:A", "v")));
    }
}
