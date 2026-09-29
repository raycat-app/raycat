#!/usr/bin/env python3
"""Записывает запросы подписки настоящих приложений байт в байт (только stdlib).

Каждый запрос сохраняется как есть (заголовки и тело, CRLF) в <out>/NN.http, краткая
строка идёт в stdout. В ответ уходит небольшая валидная подписка (base64 из ссылок
и обычные заголовки провайдера), чтобы приложение приняло её и продолжило работу.

    python3 capture_server.py --port 18080 --out captures
"""

import argparse
import base64
import os
import socketserver
import sys
import threading
import time

LINKS = "\n".join([
    "vless://00000000-0000-0000-0000-000000000001@203.0.113.21:443?security=none&type=tcp#Capture-1",
    "ss://YWVzLTEyOC1nY206Y2FwdHVyZQ@203.0.113.22:8388#Capture-2",
])
HEADERS = [
    ("content-type", "text/plain; charset=utf-8"),
    ("subscription-userinfo", "upload=0; download=0; total=107374182400; expire=0"),
    ("profile-title", "base64:" + base64.b64encode(b"Capture").decode()),
    ("profile-update-interval", "24"),
]
MAX_HEAD = 65536


class Handler(socketserver.StreamRequestHandler):
    counter = 0
    lock = threading.Lock()
    timeout = 30

    def handle(self):
        head = b""
        while not head.endswith(b"\r\n\r\n") and not head.endswith(b"\n\n"):
            byte = self.rfile.read(1)
            if not byte:
                break
            head += byte
            if len(head) > MAX_HEAD:
                break
        body = b""
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                try:
                    length = int(line.split(b":", 1)[1].strip() or 0)
                except ValueError:
                    length = 0
                body = self.rfile.read(min(length, MAX_HEAD))
        with Handler.lock:
            Handler.counter += 1
            number = Handler.counter
        with open(os.path.join(self.server.out, f"{number:02d}.http"), "wb") as f:
            f.write(head + body)
        lines = head.decode("latin-1", "replace").split("\r\n")
        agent = next((x for x in lines if x.lower().startswith("user-agent:")), "")
        print(f"{time.strftime('%H:%M:%S')} #{number} {self.client_address[0]} {lines[0]} | {agent}", flush=True)
        payload = base64.b64encode(LINKS.encode())
        response = b"HTTP/1.1 200 OK\r\n"
        for name, value in HEADERS:
            response += f"{name}: {value}\r\n".encode()
        response += f"content-length: {len(payload)}\r\nconnection: close\r\n\r\n".encode()
        self.wfile.write(response + payload)


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=18080)
    parser.add_argument("--out", default="captures")
    args = parser.parse_args()
    # Кодировка Windows-консоли по умолчанию не знает русских букв, и сервер падал при старте.
    sys.stdout.reconfigure(encoding="utf-8")
    os.makedirs(args.out, exist_ok=True)
    server = Server((args.host, args.port), Handler)
    server.out = args.out
    print(f"запись на {args.host}:{args.port} в {args.out}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
