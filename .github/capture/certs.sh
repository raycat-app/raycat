#!/usr/bin/env bash
# Одноразовые сертификаты для приложений, которые принимают подписку только по HTTPS
# (INCY запрещает http в сетевой конфигурации): свой корневой ЦС и сертификат сервера
# для localhost, 127.0.0.1 и 10.0.2.2. Корневой ЦС ставится в систему эмулятора.
#   bash certs.sh <каталог>   ->  ca.pem, server.pem, server.key
set -euo pipefail
dir=$1
mkdir -p "$dir"
cd "$dir"
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 3 \
  -subj "/CN=Capture CA" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"
openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj "/CN=localhost"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1,IP:10.0.2.2\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' >ext.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem -days 3 -extfile ext.cnf
