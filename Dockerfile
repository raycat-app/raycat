# Образ собирается из готового статического бинарника raycat: dist/<amd64|arm64>/raycat.
# Версия и sha256 архива xray — в versions.toml, имя архива зависит от архитектуры.
# Готовый набор аргументов печатает .github/build/docker-args.sh:
#   docker build -t raycat \
#     --build-arg XRAY_VERSION=26.9.30 \
#     --build-arg XRAY_ARCHIVE=Xray-linux-64.zip \
#     --build-arg XRAY_SHA256=<sha256 архива> .
#
# XRAY_BUILD=noaes (только arm64 и armv7) собирает xray для процессоров без аппаратного AES.
# Сначала из исходников тега GO_VERSION собирается тулчейн Go с патчем .github/build/go-noaes.patch
# (он отменяет медленный константный GHASH), затем им собирается xray. XRAY_VERSION, XRAY_COMMIT,
# GO_VERSION, GO_COMMIT и GO_IMAGE берутся из секции [xray.noaes] файла versions.toml. Только бинарник xray:
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

# Тулчейн не зависит от версии xray, поэтому его слой кэшируется отдельно: смена xray его не пересобирает.
# Сборка идёт на платформе раннера: сам xray потом компилируется под GOARCH целевой архитектуры.
# hadolint ignore=DL3029
FROM --platform=$BUILDPLATFORM ${GO_IMAGE} AS go-noaes
ARG GO_VERSION
ARG GO_COMMIT
WORKDIR /usr/local/go-noaes
COPY .github/build/go-noaes.patch /go-noaes.patch
# Версии пакетов не закреплены: репозиторий Alpine хранит только текущие сборки.
# Суффикс в VERSION попадает в runtime.Version: `xray version` печатает его, и CI по нему видит,
# что xray собран тулчейном с патчем. make.bash — скрипт bash, поэтому bash ставится здесь же.
# hadolint ignore=DL3018
RUN --mount=type=cache,target=/root/.cache/go-build \
    set -eu; \
    : "${GO_VERSION:?}" "${GO_COMMIT:?}"; \
    apk add --no-cache bash git; \
    git init -q .; \
    git remote add origin https://github.com/golang/go.git; \
    git fetch -q --depth 1 origin "${GO_COMMIT}"; \
    git checkout -q FETCH_HEAD; \
    test "$(git rev-parse HEAD)" = "${GO_COMMIT}"; \
    git apply /go-noaes.patch; \
    printf 'go%s-noaes\n' "${GO_VERSION}" > VERSION; \
    cd src; \
    CGO_ENABLED=0 GOROOT_BOOTSTRAP=/usr/local/go ./make.bash; \
    cd ..; \
    rm -rf .git; \
    test "$(/usr/local/go-noaes/bin/go env GOVERSION)" = "go${GO_VERSION}-noaes"

# hadolint ignore=DL3029
FROM --platform=$BUILDPLATFORM ${GO_IMAGE} AS xray-noaes-build
ARG XRAY_COMMIT
ARG GO_VERSION
ARG TARGETARCH
ARG TARGETVARIANT
COPY --from=go-noaes /usr/local/go-noaes /usr/local/go-noaes
WORKDIR /src
# Версии пакетов не закреплены: репозиторий Alpine хранит только текущие сборки.
# hadolint ignore=DL3018
RUN set -eu; \
    : "${XRAY_COMMIT:?}" "${GO_VERSION:?}"; \
    apk add --no-cache git; \
    git init -q .; \
    git remote add origin https://github.com/XTLS/Xray-core.git; \
    git fetch -q --depth 1 origin "${XRAY_COMMIT}"; \
    git checkout -q FETCH_HEAD; \
    test "$(git rev-parse HEAD)" = "${XRAY_COMMIT}"
ENV GOROOT=/usr/local/go-noaes \
    PATH=/usr/local/go-noaes/bin:${PATH} \
    CGO_ENABLED=0 \
    GOTOOLCHAIN=local
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
    test "$(go env GOVERSION)" = "go${GO_VERSION}-noaes"; \
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
