# Образ собирается из готового статического бинарника raycat: dist/<amd64|arm64>/raycat.
# Версия и sha256 архива xray — в versions.toml, имя архива зависит от архитектуры:
#   docker build -t raycat \
#     --build-arg XRAY_VERSION=26.9.9 \
#     --build-arg XRAY_ARCHIVE=Xray-linux-64.zip \
#     --build-arg XRAY_SHA256=<sha256 архива> .

ARG ALPINE_IMAGE=alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

FROM ${ALPINE_IMAGE} AS xray
ARG XRAY_VERSION
ARG XRAY_ARCHIVE
ARG XRAY_SHA256
ADD --checksum=sha256:${XRAY_SHA256} \
    https://github.com/XTLS/Xray-core/releases/download/v${XRAY_VERSION}/${XRAY_ARCHIVE} /xray.zip
RUN unzip -q /xray.zip -d /out xray

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
ENV RAYCAT_STATE_DIR=/var/lib/raycat
ENTRYPOINT ["raycat"]
CMD ["daemon"]
