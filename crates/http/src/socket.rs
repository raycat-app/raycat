//! Сокеты с меткой `SO_MARK`. Весь `unsafe` крейта находится здесь.
//!
//! Метку нужно поставить до SYN: у `std` нет способа настроить сокет до `connect`,
//! поэтому сокет создаётся и подключается через libc.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn len_of<T>() -> io::Result<libc::socklen_t> {
    libc::socklen_t::try_from(size_of::<T>()).map_err(io::Error::other)
}

fn family(af: libc::c_int) -> io::Result<libc::sa_family_t> {
    libc::sa_family_t::try_from(af).map_err(io::Error::other)
}

#[allow(unsafe_code)]
fn open(af: libc::c_int, kind: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: системный вызов без указателей.
    let raw = check(unsafe { libc::socket(af, kind | libc::SOCK_CLOEXEC, 0) })?;
    // SAFETY: `raw` только что создан, других владельцев у дескриптора нет.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

#[allow(unsafe_code)]
fn set_mark(fd: RawFd, mark: u32) -> io::Result<()> {
    let len = len_of::<u32>()?;
    // SAFETY: указатель на `mark` и его точный размер действительны на время вызова.
    check(unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_MARK,
            (&raw const mark).cast(),
            len,
        )
    })?;
    Ok(())
}

#[allow(unsafe_code)]
fn connect_struct<T>(fd: RawFd, addr: &T) -> io::Result<()> {
    let len = len_of::<T>()?;
    // SAFETY: `T` здесь всегда `sockaddr_in` или `sockaddr_in6`, полностью
    // заполненная структура передаётся вместе со своим точным размером.
    check(unsafe { libc::connect(fd, std::ptr::from_ref(addr).cast(), len) })?;
    Ok(())
}

#[allow(unsafe_code)]
fn wait_writable(fd: RawFd, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis())
        .unwrap_or(libc::c_int::MAX)
        .max(1);
    // SAFETY: `pfd` — одна действительная структура, nfds равен 1.
    let ready = check(unsafe { libc::poll(&raw mut pfd, 1, millis) })?;
    Ok(ready != 0)
}

#[allow(unsafe_code)]
fn socket_error(fd: RawFd) -> io::Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut len = len_of::<libc::c_int>()?;
    // SAFETY: `value` и `len` действительны на время вызова, `len` равен размеру `value`.
    check(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut value).cast(),
            &raw mut len,
        )
    })?;
    Ok(value)
}

fn connect_raw(fd: RawFd, addr: &SocketAddr) -> io::Result<()> {
    match addr {
        SocketAddr::V4(a) => connect_struct(
            fd,
            &libc::sockaddr_in {
                sin_family: family(libc::AF_INET)?,
                sin_port: a.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(a.ip().octets()),
                },
                sin_zero: [0; 8],
            },
        ),
        SocketAddr::V6(a) => connect_struct(
            fd,
            &libc::sockaddr_in6 {
                sin6_family: family(libc::AF_INET6)?,
                sin6_port: a.port().to_be(),
                sin6_flowinfo: a.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: a.ip().octets(),
                },
                sin6_scope_id: a.scope_id(),
            },
        ),
    }
}

/// Новый сокет типа `kind` (`SOCK_STREAM`, `SOCK_DGRAM`) с меткой `SO_MARK`: по метке
/// kill switch шлюза пропускает пакеты наружу.
pub(crate) fn marked_socket(
    addr: &SocketAddr,
    kind: libc::c_int,
    mark: u32,
) -> io::Result<OwnedFd> {
    let af = if addr.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let fd = open(af, kind)?;
    if let Err(err) = set_mark(fd.as_raw_fd(), mark)
        // EPERM: нет CAP_NET_ADMIN; ENOPROTOOPT: например, эмуляция qemu-user.
        // Без метки соединение всё равно работает там, где kill switch не включён.
        && !matches!(err.raw_os_error(), Some(libc::EPERM | libc::ENOPROTOOPT))
    {
        return Err(err);
    }
    Ok(fd)
}

/// `TcpStream::connect_timeout`, но с меткой на сокете до отправки SYN.
pub(crate) fn connect_marked(
    addr: &SocketAddr,
    timeout: Duration,
    mark: u32,
) -> io::Result<TcpStream> {
    let fd = marked_socket(addr, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, mark)?;
    if let Err(err) = connect_raw(fd.as_raw_fd(), addr) {
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        if !wait_writable(fd.as_raw_fd(), timeout)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "тайм-аут подключения",
            ));
        }
        let pending = socket_error(fd.as_raw_fd())?;
        if pending != 0 {
            return Err(io::Error::from_raw_os_error(pending));
        }
    }
    let stream = TcpStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn marked_connections_work() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Без CAP_NET_ADMIN метка пропускается, с ним — ставится; в обоих случаях
        // соединение должно вести себя как обычное.
        let mut stream = connect_marked(&addr, Duration::from_secs(2), 0x1234).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        stream.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        drop(listener);
        let closed = connect_marked(&addr, Duration::from_secs(2), 1).unwrap_err();
        assert_eq!(closed.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[test]
    fn marked_ipv6_connections_work() {
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            // В окружении нет IPv6 на loopback.
            return;
        };
        let addr = listener.local_addr().unwrap();
        let mut stream = connect_marked(&addr, Duration::from_secs(2), 0x1234).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        stream.write_all(b"ok").unwrap();
        let mut buf = [0u8; 2];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ok");
    }
}
