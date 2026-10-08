# Образ собирается из готового статического бинарника raycat: dist/<amd64|arm64>/raycat.
# Версия и sha256 архива xray — в versions.toml, имя архива зависит от архитектуры.
# Готовый набор аргументов печатает .github/build/docker-args.sh:
#   docker build -t raycat \
#     --build-arg XRAY_VERSION=26.9.30 \
#     --build-arg XRAY_ARCHIVE=Xray-linux-64.zip \
#     --build-arg XRAY_SHA256=<sha256 архива> .
#
# XRAY_BUILD=noaes (только arm64 и armv7) собирает xray из исходников на Go 1.26 для
# процессоров без аппаратного AES; XRAY_COMMIT, GO_VERSION, GO_IMAGE и XRAY_GO_DIRECTIVE
# берутся из секции [xray.noaes] файла versions.toml. Только бинарник xray:
#   docker build --platform linux/arm/v7 --target xray-noaes --output type=local,dest=out ...

ARG ALPINE_IMAGE=alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
ARG GO_IMAGE=scratch
ARG XRAY_BUILD=release

FROM ${ALPINE_IMAGE} AS xray-release
ARG XRAY_VERSION
ARG XRAY_ARCHIVE
ARG XRAY_SHA256
ADD --checksum=sha256:${XRAY_SHA256} \
    https://github.com/XTLS/Xray-core/releases/download/v${XRAY_VERSION}/${XRAY_ARCHIVE} /xray.zip
RUN unzip -q /xray.zip -d /out xray

# Сборка идёт на платформе раннера, целевая архитектура задаётся GOARCH.
# hadolint ignore=DL3029
FROM --platform=$BUILDPLATFORM ${GO_IMAGE} AS xray-noaes-build
ARG XRAY_COMMIT
ARG GO_VERSION
ARG XRAY_GO_DIRECTIVE
ARG TARGETARCH
ARG TARGETVARIANT
WORKDIR /src
# Версии пакетов не закреплены: репозиторий Alpine хранит только текущие сборки.
# go.mod апстрима требует Go 1.27 из-за директивы go, а не из-за API: копия go.mod
# отличается от оригинала только этой строкой, go.sum берётся без изменений.
# hadolint ignore=DL3018
RUN set -eu; \
    : "${XRAY_COMMIT:?}" "${GO_VERSION:?}" "${XRAY_GO_DIRECTIVE:?}"; \
    apk add --no-cache git; \
    git init -q .; \
    git remote add origin https://github.com/XTLS/Xray-core.git; \
    git fetch -q --depth 1 origin "${XRAY_COMMIT}"; \
    git checkout -q FETCH_HEAD; \
    test "$(git rev-parse HEAD)" = "${XRAY_COMMIT}"; \
    grep -qx "go ${XRAY_GO_DIRECTIVE}" go.mod; \
    test "$(grep -c '^toolchain ' go.mod)" -eq 0; \
    sed "s/^go ${XRAY_GO_DIRECTIVE}\$/go ${GO_VERSION%.*}/" go.mod > /noaes.mod; \
    cp go.sum /noaes.sum; \
    grep -qx "go ${GO_VERSION%.*}" /noaes.mod; \
    grep -v '^go ' go.mod > /upstream.rest; \
    grep -v '^go ' /noaes.mod > /noaes.rest; \
    cmp /upstream.rest /noaes.rest
ENV CGO_ENABLED=0 GOTOOLCHAIN=local GOFLAGS=-modfile=/noaes.mod
# Кэши Go в монтировании, а не в слое: слой остаётся размером с бинарник, и его дёшево
# хранить в кэше сборки CI.
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    set -eu; \
    case "${TARGETARCH:-}${TARGETVARIANT:-}" in \
      arm64|arm64v8) GOARCH=arm64; GOARM= ;; \
      armv7) GOARCH=arm; GOARM=7 ;; \
      *) echo "noaes: платформа ${TARGETARCH:-}${TARGETVARIANT:-} не поддерживается" >&2; exit 1 ;; \
    esac; \
    export GOARCH GOARM; \
    test "$(go env GOVERSION)" = "go${GO_VERSION}"; \
    go mod download; \
    build=$(git describe --always --dirty); \
    go build -o /out/xray -trimpath -buildvcs=false -gcflags="all=-l=4" \
      -ldflags="-X github.com/xtls/xray-core/core.build=${build} -s -w -buildid=" ./main

FROM scratch AS xray-noaes
COPY --from=xray-noaes-build /out/xray /out/xray

# hadolint ignore=DL3006
FROM xray-${XRAY_BUILD} AS xray

FROM ${ALPINE_IMAGE}
# Версии пакетов не закреплены: репозиторий Alpine хранит только текущие сборки.
# hadolint ignore=DL3018
RUN apk add --no-cache nftables iproute2 ca-certificates tzdata
ARG TARGETARCH
COPY --chmod=755 dist/${TARGETARCH}/raycat /usr/local/bin/raycat
COPY --from=xray --chmod=755 /out/xray /usr/libexec/raycat/xray
LABEL org.opencontainers.image.title="raycat" \
      org.opencontainers.image.description="Серверный клиент VPN-подписок: шлюз и прокси для хоста и Docker-контейнеров" \
      org.opencontainers.image.source="https://github.com/raycat-app/raycat" \
      org.opencontainers.image.licenses="MIT"
# Сокет API лежит в каталоге состояния: при read_only: true записываемым остаётся
# только он (том), а /run, где сокет root лежит по умолчанию, доступен только для чтения.
ENV RAYCAT_STATE_DIR=/var/lib/raycat \
    RAYCAT_SOCKET=/var/lib/raycat/raycat.sock
# Окно в 60 с до первого успеха не считается неудачей (медленная панель при первом
# старте), затем ещё три неудачи по 10 с до статуса unhealthy.
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s --retries=3 CMD ["raycat", "health"]
ENTRYPOINT ["raycat"]
CMD ["daemon"]
