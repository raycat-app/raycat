//! Клиент против локальных фейковых серверов на 127.0.0.1: в интернет тесты не ходят.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::GzEncoder;
use raycat_http::{Client, Request, Url};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

/// Читает запрос целиком: заголовки до пустой строки и тело по `Content-Length`.
fn read_request(sock: &mut TcpStream) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        sock.read_exact(&mut byte).unwrap();
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    if let Some(len) = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
    {
        let mut body = vec![0u8; len.trim().parse().unwrap()];
        sock.read_exact(&mut body).unwrap();
        raw.extend_from_slice(&body);
    }
    raw
}

fn accept(listener: &TcpListener) -> TcpStream {
    let (sock, _) = listener.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    sock
}

/// Отвечает на каждое следующее соединение очередным готовым ответом и возвращает
/// принятые запросы.
fn serve(responses: Vec<Vec<u8>>) -> (u16, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for response in responses {
            let mut sock = accept(&listener);
            seen.push(String::from_utf8(read_request(&mut sock)).unwrap());
            sock.write_all(&response).unwrap();
        }
        seen
    });
    (port, handle)
}

/// Принимает запрос и молчит, пока клиент не потеряет терпение.
fn serve_silence() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let mut sock = accept(&listener);
        read_request(&mut sock);
        thread::sleep(Duration::from_millis(1500));
    });
    port
}

fn url(port: u16) -> Url {
    Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
}

fn headers(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn get(client: &Client, url: &Url) -> anyhow::Result<raycat_http::Response> {
    client.send(
        url,
        &Request {
            method: "GET",
            target: "/",
            headers: &headers(&[("Host", "h")]),
            body: &[],
        },
    )
}

fn err_text(result: anyhow::Result<raycat_http::Response>) -> String {
    format!("{:#}", result.unwrap_err())
}

#[test]
fn request_is_written_verbatim() {
    let (port, server) = serve(vec![OK.to_vec()]);
    let sent = headers(&[
        ("host", format!("127.0.0.1:{port}").as_str()),
        ("X-Weird-CASE", "V a l"),
        ("accept", "*/*"),
        ("User-Agent", "ua/1"),
    ]);
    let response = Client::default()
        .send(
            &url(port),
            &Request {
                method: "GET",
                target: "/sub/abc?x=1",
                headers: &sent,
                body: &[],
            },
        )
        .unwrap();
    assert_eq!((response.status, response.body), (200, b"ok".to_vec()));
    let expected = format!(
        "GET /sub/abc?x=1 HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nX-Weird-CASE: V a l\r\n\
         accept: */*\r\nUser-Agent: ua/1\r\n\r\n"
    );
    assert_eq!(server.join().unwrap(), [expected]);
}

#[test]
fn content_length_is_added_only_for_a_body() {
    let no_content = || b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
    let (port, server) = serve(vec![no_content(), no_content(), no_content()]);
    let client = Client::default();
    let target = url(port);
    let send = |method: &str, sent: &[(String, String)], body: &[u8]| {
        client
            .send(
                &target,
                &Request {
                    method,
                    target: "/",
                    headers: sent,
                    body,
                },
            )
            .unwrap();
    };
    send("POST", &headers(&[("Host", "h")]), b"abc");
    send(
        "POST",
        &headers(&[("Host", "h"), ("content-length", "3")]),
        b"abc",
    );
    send("GET", &headers(&[("Host", "h")]), b"");
    assert_eq!(
        server.join().unwrap(),
        [
            "POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\n\r\nabc",
            "POST / HTTP/1.1\r\nHost: h\r\ncontent-length: 3\r\n\r\nabc",
            "GET / HTTP/1.1\r\nHost: h\r\n\r\n",
        ]
    );
}

#[test]
fn reads_every_kind_of_body() {
    let mut gz = Vec::new();
    {
        let mut encoder = GzEncoder::new(&mut gz, Compression::default());
        encoder.write_all(b"proxies: []").unwrap();
    }
    let mut gzipped = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
        gz.len()
    )
    .into_bytes();
    gzipped.extend_from_slice(&gz);
    let (port, server) = serve(vec![
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
            .to_vec(),
        b"HTTP/1.0 200 OK\r\n\r\nuntil close".to_vec(),
        gzipped,
    ]);
    let client = Client::default();
    let target = url(port);
    assert_eq!(get(&client, &target).unwrap().body, b"hello world");
    assert_eq!(get(&client, &target).unwrap().body, b"until close");
    assert_eq!(get(&client, &target).unwrap().body, b"proxies: []");
    server.join().unwrap();
}

#[test]
fn head_responses_have_no_body() {
    let (port, server) = serve(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nX-A: b\r\n\r\n".to_vec(),
    ]);
    let response = Client::default()
        .send(
            &url(port),
            &Request {
                method: "HEAD",
                target: "/",
                headers: &headers(&[("Host", "h")]),
                body: &[],
            },
        )
        .unwrap();
    assert_eq!(response.body, Vec::<u8>::new());
    assert_eq!(response.header("x-a"), Some("b"));
    server.join().unwrap();
}

#[test]
fn redirects_are_left_to_the_caller() {
    let (port, server) = serve(vec![
        b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.example/x\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    ]);
    let response = get(&Client::default(), &url(port)).unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(
        response.header("Location"),
        Some("https://elsewhere.example/x")
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn reports_the_peer_address() {
    let (port, server) = serve(vec![OK.to_vec()]);
    let response = get(&Client::default(), &url(port)).unwrap();
    assert_eq!(response.peer, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    server.join().unwrap();
}

#[test]
fn a_marked_client_still_connects() {
    let (port, server) = serve(vec![OK.to_vec()]);
    let client = Client {
        mark: Some(0x1234),
        ..Client::default()
    };
    assert_eq!(get(&client, &url(port)).unwrap().body, b"ok");
    server.join().unwrap();
}

#[test]
fn connects_through_a_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = thread::spawn(move || {
        let mut sock = accept(&listener);
        let connect = read_request(&mut sock);
        sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
        let inner = read_request(&mut sock);
        sock.write_all(OK).unwrap();
        (connect, inner)
    });
    let client = Client {
        proxy: Some(url(port)),
        ..Client::default()
    };
    let target = Url::parse("http://203.0.113.7:8080/x").unwrap();
    let response = client
        .send(
            &target,
            &Request {
                method: "GET",
                target: &target.target,
                headers: &headers(&[("Host", target.host_header().as_str())]),
                body: &[],
            },
        )
        .unwrap();
    assert_eq!(response.body, b"ok");
    assert_eq!(response.peer, None);
    let (connect, inner) = proxy.join().unwrap();
    assert_eq!(
        connect,
        b"CONNECT 203.0.113.7:8080 HTTP/1.1\r\nHost: 203.0.113.7:8080\r\n\r\n"
    );
    assert_eq!(inner, b"GET /x HTTP/1.1\r\nHost: 203.0.113.7:8080\r\n\r\n");
}

#[test]
fn a_refused_tunnel_is_an_error() {
    let (port, server) = serve(vec![
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    let client = Client {
        proxy: Some(url(port)),
        ..Client::default()
    };
    let target = Url::parse("http://203.0.113.7/").unwrap();
    let text = err_text(get(&client, &target));
    assert!(text.contains("прокси отклонил CONNECT"), "{text}");
    assert!(text.contains("403"), "{text}");
    server.join().unwrap();
}

#[test]
fn an_https_proxy_is_refused() {
    let client = Client {
        proxy: Some(Url::parse("https://127.0.0.1:1/").unwrap()),
        ..Client::default()
    };
    let text = err_text(get(&client, &url(1)));
    assert!(text.contains("прокси по https"), "{text}");
}

#[test]
fn the_idle_timeout_names_itself() {
    let client = Client {
        io_timeout: Duration::from_millis(200),
        ..Client::default()
    };
    let started = Instant::now();
    let text = err_text(get(&client, &url(serve_silence())));
    assert!(text.contains("сервер не отвечает уже 200ms"), "{text}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_total_deadline_names_itself() {
    let client = Client {
        total_timeout: Duration::from_millis(300),
        io_timeout: Duration::from_secs(5),
        ..Client::default()
    };
    let started = Instant::now();
    let text = err_text(get(&client, &url(serve_silence())));
    assert!(text.contains("истёк общий срок запроса"), "{text}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_body_limit_is_enforced() {
    let mut big = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec();
    big.extend_from_slice(&[b'x'; 100]);
    let (port, _server) = serve(vec![big]);
    let client = Client {
        max_body: 10,
        ..Client::default()
    };
    let text = err_text(get(&client, &url(port)));
    assert!(text.contains("лимита"), "{text}");
}

#[test]
fn a_closed_port_is_a_connect_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let text = err_text(get(&Client::default(), &url(port)));
    assert!(
        text.contains(&format!("подключение к 127.0.0.1:{port}")),
        "{text}"
    );
}

#[test]
fn line_breaks_in_fields_never_reach_the_wire() {
    let client = Client::default();
    let target = url(1);
    let send = |method: &str, path: &str, sent: &[(String, String)]| {
        err_text(client.send(
            &target,
            &Request {
                method,
                target: path,
                headers: sent,
                body: &[],
            },
        ))
    };
    for text in [
        send("GET", "/", &headers(&[("X-A", "v\r\nX-B: w")])),
        send("GET", "/ HTTP/1.1\r\nX-B: w", &[]),
        send("GET\r\n", "/", &[]),
    ] {
        assert!(text.contains("недопустимый символ"), "{text}");
    }
}

#[test]
fn streams_pieces_and_stops_when_asked() {
    let chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
            .to_vec();
    let sized = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789".to_vec();
    let (port, server) = serve(vec![chunked, sized]);
    let client = Client::default();
    let target = url(port);
    let sent = headers(&[("Host", "h")]);
    let request = Request {
        method: "GET",
        target: "/",
        headers: &sent,
        body: &[],
    };

    let mut all = Vec::new();
    let response = client
        .stream(&target, &request, &mut |piece| {
            all.extend_from_slice(piece);
            true
        })
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, Vec::<u8>::new());
    assert_eq!(all, b"hello world");

    let mut seen = 0;
    client
        .stream(&target, &request, &mut |piece| {
            seen += piece.len();
            false
        })
        .unwrap();
    assert!((1..=10).contains(&seen), "{seen}");
    server.join().unwrap();
}

#[test]
fn streams_through_a_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = thread::spawn(move || {
        let mut sock = accept(&listener);
        read_request(&mut sock);
        sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
        let inner = read_request(&mut sock);
        sock.write_all(OK).unwrap();
        inner
    });
    let client = Client {
        proxy: Some(url(port)),
        ..Client::default()
    };
    let target = Url::parse("http://203.0.113.7:8080/x").unwrap();
    let mut body = Vec::new();
    client
        .stream(
            &target,
            &Request {
                method: "GET",
                target: &target.target,
                headers: &headers(&[("Host", target.host_header().as_str())]),
                body: &[],
            },
            &mut |piece| {
                body.extend_from_slice(piece);
                true
            },
        )
        .unwrap();
    assert_eq!(body, b"ok");
    assert_eq!(
        proxy.join().unwrap(),
        b"GET /x HTTP/1.1\r\nHost: 203.0.113.7:8080\r\n\r\n"
    );
}

#[test]
fn the_tunnel_needs_the_proxy_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = thread::spawn(move || {
        let mut answers = Vec::new();
        for _ in 0..2 {
            let mut sock = accept(&listener);
            let connect = String::from_utf8(read_request(&mut sock)).unwrap();
            if connect.contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n") {
                sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .unwrap();
                read_request(&mut sock);
                sock.write_all(OK).unwrap();
            } else {
                sock.write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
            }
            answers.push(connect);
        }
        answers
    });
    let target = Url::parse("http://203.0.113.7/").unwrap();
    let sent = headers(&[("Host", "203.0.113.7")]);
    let request = || Request {
        method: "GET",
        target: "/",
        headers: &sent,
        body: &[],
    };
    let anonymous = Client {
        proxy: Some(url(port)),
        ..Client::default()
    };
    let text = err_text(anonymous.send(&target, &request()));
    assert!(text.contains("407"), "{text}");

    let authorized = Client {
        proxy: Some(url(port)),
        proxy_authorization: Some("Basic dXNlcjpwYXNz".to_owned()),
        ..Client::default()
    };
    assert_eq!(authorized.send(&target, &request()).unwrap().body, b"ok");
    let answers = proxy.join().unwrap();
    assert!(!answers[0].contains("Proxy-Authorization"));
    assert!(answers[1].contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n"));
}

#[test]
fn compressed_bodies_are_refused_when_streaming() {
    let (port, server) = serve(vec![
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\nxx".to_vec(),
    ]);
    let text = err_text(Client::default().stream(
        &url(port),
        &Request {
            method: "GET",
            target: "/",
            headers: &headers(&[("Host", "h")]),
            body: &[],
        },
        &mut |_| true,
    ));
    assert!(text.contains("сжатое тело"), "{text}");
    server.join().unwrap();
}
