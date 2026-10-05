# syntax=docker/dockerfile:1

########################################################################
# 1. Generate the static website.
########################################################################
FROM debian:bookworm-slim AS site-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY . /src/iuna-work

COPY www /site
COPY wallet /site/wallet
COPY downloads /site/downloads
RUN set -eux; \
    cp /src/iuna-work/src-tauri/icons/icon.svg /site/favicon.svg; \
    cp /src/iuna-work/src-tauri/icons/icon.ico /site/favicon.ico; \
    cp /src/iuna-work/src-tauri/icons/128x128@2x.png /site/apple-touch-icon.png; \
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' /src/iuna-work/Cargo.toml | head -n 1)"; \
    mkdir -p /site/downloads; \
    cp /site/downloads.html /site/downloads/index.html; \
    sed -i "s|\${IUNA_VERSION}|${version}|g" /site/downloads/index.html; \
    if [ ! -f /site/downloads/latest.json ]; then printf '{"tag":"v%s","version":"%s","url":"https://getiuna.org/downloads/"}\n' "$version" "$version" > /site/downloads/latest.json; fi; \
    rm -f /site/downloads.html; \
    python3 /src/iuna-work/scripts/inject_cloudflare_analytics.py /site

########################################################################
# 2. Ship it - plain nginx, static files only.
########################################################################
FROM nginx:1.27-alpine

COPY --from=site-builder /site /usr/share/nginx/html
COPY nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80
