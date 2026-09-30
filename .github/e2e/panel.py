#!/usr/bin/env python3
"""Панель подписок для e2e-стенда: отдаёт тело из файла и записывает запросы."""

import argparse
import http.server
import pathlib


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--body", type=pathlib.Path, required=True)
    parser.add_argument("--requests", type=pathlib.Path, required=True)
    args = parser.parse_args()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            with args.requests.open("a", encoding="utf-8") as log:
                log.write(f"{self.command} {self.path}\n")
                for name, value in self.headers.items():
                    log.write(f"{name}: {value}\n")
                log.write("\n")
            body = args.body.read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("profile-title", "E2E")
            self.send_header(
                "subscription-userinfo",
                "upload=1024; download=2048; total=1073741824; expire=1893456000",
            )
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    http.server.ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
