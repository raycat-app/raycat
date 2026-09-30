#!/usr/bin/env python3
"""Заглушка для reality в бенчмарке: локальный TLS 1.3 сервер вместо внешнего сайта."""

import argparse
import http.server
import ssl


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    args = parser.parse_args()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"ok")

        def log_message(self, *_):
            pass

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(args.cert, args.key)
    context.set_alpn_protocols(["h2", "http/1.1"])
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    # Рукопожатие в потоке соединения: зависший клиент не должен блокировать accept.
    server.socket = context.wrap_socket(
        server.socket, server_side=True, do_handshake_on_connect=False
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
