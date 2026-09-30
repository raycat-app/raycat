#!/usr/bin/env python3
"""Сервер и клиент стенда правил nftables (.github/e2e/netfilter.sh).

  serve tcp|udp --bind АДРЕС --port N --label ИМЯ [--transparent] [--log ФАЙЛ]
  probe tcp|udp АДРЕС PORT [--mark N] [--timeout СЕК]

Сервер отвечает строкой «ИМЯ адрес:порт», где адрес и порт — те, на которые клиент
отправил пакет. Прозрачный сервер (IP_TRANSPARENT) играет роль xray: он принимает
перехваченные соединения с чужим адресом назначения и по ответу видно, куда клиент
хотел попасть. Клиент печатает ответ; код 1 — ответа нет.
"""

import argparse
import socket
import struct
import sys

SOL_IP = 0
IP_TRANSPARENT = 19
IP_RECVORIGDSTADDR = 20
SO_MARK = 36


def family(address):
    return socket.AF_INET6 if ":" in address else socket.AF_INET


def note(path, text):
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(text + "\n")


def serve_tcp(args):
    sock = socket.socket(family(args.bind), socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if args.transparent:
        sock.setsockopt(SOL_IP, IP_TRANSPARENT, 1)
    sock.bind((args.bind, args.port))
    sock.listen(16)
    while True:
        conn, peer = sock.accept()
        local = conn.getsockname()
        note(args.log, f"tcp {peer[0]} -> {local[0]}:{local[1]}")
        conn.sendall(f"{args.label} {local[0]}:{local[1]}\n".encode())
        conn.close()


def serve_udp(args):
    sock = socket.socket(family(args.bind), socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if args.transparent:
        sock.setsockopt(SOL_IP, IP_TRANSPARENT, 1)
        sock.setsockopt(SOL_IP, IP_RECVORIGDSTADDR, 1)
    sock.bind((args.bind, args.port))
    while True:
        _, ancillary, _, peer = sock.recvmsg(2048, 1024)
        target = (args.bind, args.port)
        for level, kind, payload in ancillary:
            if level == SOL_IP and kind == IP_RECVORIGDSTADDR:
                port = struct.unpack("!H", payload[2:4])[0]
                target = (socket.inet_ntoa(payload[4:8]), port)
        note(args.log, f"udp {peer[0]} -> {target[0]}:{target[1]}")
        reply = f"{args.label} {target[0]}:{target[1]}\n".encode()
        if args.transparent:
            # Ответ должен прийти с адреса, на который клиент отправлял пакет.
            out = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            out.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            out.setsockopt(SOL_IP, IP_TRANSPARENT, 1)
            out.bind(target)
            out.sendto(reply, peer)
            out.close()
        else:
            sock.sendto(reply, peer)


def probe(args):
    kind = socket.SOCK_STREAM if args.proto == "tcp" else socket.SOCK_DGRAM
    sock = socket.socket(family(args.address), kind)
    sock.settimeout(args.timeout)
    if args.mark:
        sock.setsockopt(socket.SOL_SOCKET, SO_MARK, int(args.mark, 0))
    try:
        sock.connect((args.address, args.port))
        if args.proto == "udp":
            sock.send(b"ping")
        data = sock.recv(256)
    except OSError as error:
        print(f"{args.proto} {args.address}:{args.port}: {error}", file=sys.stderr)
        return 1
    finally:
        sock.close()
    if not data:
        print(f"{args.proto} {args.address}:{args.port}: пустой ответ", file=sys.stderr)
        return 1
    sys.stdout.write(data.decode())
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    serve = commands.add_parser("serve")
    serve.add_argument("proto", choices=["tcp", "udp"])
    serve.add_argument("--bind", required=True)
    serve.add_argument("--port", type=int, required=True)
    serve.add_argument("--label", required=True)
    serve.add_argument("--transparent", action="store_true")
    serve.add_argument("--log")

    check = commands.add_parser("probe")
    check.add_argument("proto", choices=["tcp", "udp"])
    check.add_argument("address")
    check.add_argument("port", type=int)
    check.add_argument("--mark")
    check.add_argument("--timeout", type=float, default=2.0)

    args = parser.parse_args()
    if args.command == "probe":
        return probe(args)
    try:
        (serve_tcp if args.proto == "tcp" else serve_udp)(args)
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
