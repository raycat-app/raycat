#!/usr/bin/env python3
"""Долгоживущее tcp-соединение для проверок «правила не рвут чужие соединения».

Изображает ssh к хосту: сервер каждые 0.2 с шлёт «tick N» и отвечает «pong» на
«ping», клиент раз в секунду шлёт «ping» и пишет в файл состояния, что получил.

  serve --bind АДРЕС --port N
  watch АДРЕС PORT --state ФАЙЛ
  check --state ФАЙЛ [--wait СЕК]

`check` берёт два снимка состояния с паузой и требует, чтобы соединение было целым
и за паузу пришли новые tick и pong: то есть данные идут в обе стороны.
"""

import argparse
import os
import select
import socket
import sys
import threading
import time


def handle(conn):
    tick = 0
    try:
        with conn:
            while True:
                ready, _, _ = select.select([conn], [], [], 0.2)
                if ready:
                    data = conn.recv(1024)
                    if not data:
                        return
                    if b"ping" in data:
                        conn.sendall(b"pong\n")
                else:
                    tick += 1
                    conn.sendall(f"tick {tick}\n".encode())
    except OSError:
        return


def serve(args):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((args.bind, args.port))
    sock.listen(16)
    while True:
        conn, _ = sock.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


def write_state(path, status, ticks, pongs):
    temporary = f"{path}.tmp"
    with open(temporary, "w", encoding="utf-8") as handle_:
        handle_.write(f"{status}\t{ticks}\t{pongs}\t{time.time()}\n")
    os.replace(temporary, path)


def watch(args):
    ticks = pongs = 0
    pending = b""
    last_ping = 0.0
    try:
        sock = socket.create_connection((args.address, args.port), timeout=5)
    except OSError as error:
        write_state(args.state, f"broken {error}", ticks, pongs)
        return 1
    sock.settimeout(0.5)
    write_state(args.state, "ok", ticks, pongs)
    while True:
        try:
            if time.time() - last_ping >= 1.0:
                sock.sendall(b"ping\n")
                last_ping = time.time()
            data = sock.recv(4096)
        except socket.timeout:
            data = None
        except OSError as error:
            write_state(args.state, f"broken {error}", ticks, pongs)
            return 1
        if data == b"":
            write_state(args.state, "broken закрыто собеседником", ticks, pongs)
            return 1
        if data:
            pending += data
            *lines, pending = pending.split(b"\n")
            ticks += sum(1 for line in lines if line.startswith(b"tick"))
            pongs += sum(1 for line in lines if line == b"pong")
        write_state(args.state, "ok", ticks, pongs)


def read_state(path):
    with open(path, encoding="utf-8") as handle_:
        status, ticks, pongs, stamp = handle_.read().strip().split("\t")
    return status, int(ticks), int(pongs), float(stamp)


def check(args):
    try:
        first = read_state(args.state)
        time.sleep(args.wait)
        second = read_state(args.state)
    except (OSError, ValueError) as error:
        print(f"нет состояния {args.state}: {error}", file=sys.stderr)
        return 1
    if second[0] != "ok":
        print(f"соединение порвано: {second[0]}", file=sys.stderr)
        return 1
    if time.time() - second[3] > 3:
        print("наблюдатель давно не обновлял состояние", file=sys.stderr)
        return 1
    if second[1] <= first[1] or second[2] <= first[2]:
        print(
            f"данные не идут: tick {first[1]}→{second[1]}, pong {first[2]}→{second[2]}",
            file=sys.stderr,
        )
        return 1
    print(f"живо: tick {second[1]}, pong {second[2]}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    server = commands.add_parser("serve")
    server.add_argument("--bind", required=True)
    server.add_argument("--port", type=int, required=True)

    watcher = commands.add_parser("watch")
    watcher.add_argument("address")
    watcher.add_argument("port", type=int)
    watcher.add_argument("--state", required=True)

    checker = commands.add_parser("check")
    checker.add_argument("--state", required=True)
    checker.add_argument("--wait", type=float, default=2.0)

    args = parser.parse_args()
    if args.command == "serve":
        try:
            serve(args)
        except KeyboardInterrupt:
            pass
        return 0
    return (watch if args.command == "watch" else check)(args)


if __name__ == "__main__":
    sys.exit(main())
