# syntax=docker/dockerfile:1

########################################################################
# 1. Build stagit - static git page generator (log/commits/files/refs).
#    Low-resource on purpose: once generated, serving is plain static
#    files, no git process or CGI running at request time.
########################################################################
FROM debian:bookworm-slim AS stagit-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libgit2-dev \
        git \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN git clone --depth 1 https://github.com/oxalorg/stagit.git /usr/src/stagit
WORKDIR /usr/src/stagit
RUN make PREFIX=/usr/local && make PREFIX=/usr/local install

########################################################################
# 2. Generate the site: HTML browser pages + a clonable bare repo.
#
#    The build context must include .git. The .dockerignore in this repo
#    keeps it available so stagit can publish history and refs.
########################################################################
FROM stagit-builder AS site-builder

COPY . /src/iuna-work

RUN set -eux; \
    test -d /src/iuna-work/.git; \
    git clone --bare /src/iuna-work /src/iuna.git; \
    mkdir -p /site/git/iuna /var/cache/stagit-iuna; \
    echo "iuna - experimental devnet protocol" > /src/iuna.git/description; \
    echo "iuna-labs" > /src/iuna.git/owner; \
    echo "https://iuna.jhx.app/git/iuna.git" > /src/iuna.git/url; \
    cd /src/iuna.git; \
    mkdir -p /tmp/iuna-packs; \
    mv objects/pack/* /tmp/iuna-packs/; \
    for pack in /tmp/iuna-packs/*.pack; do \
        [ -e "$pack" ] || continue; \
        git unpack-objects < "$pack"; \
    done; \
    rm -rf /tmp/iuna-packs; \
    git update-server-info; \
    cd /site/git/iuna; \
    stagit -c /var/cache/stagit-iuna/cache /src/iuna.git; \
    test -f /site/git/iuna/log.html; \
    cp /site/git/iuna/log.html /site/git/iuna/index.html; \
    cd /site/git && stagit-index /src/iuna.git > index.html; \
    cp /src/iuna-work/www/assets/static-listing.css /site/git/style.css; \
    cp /src/iuna-work/www/assets/static-listing.css /site/git/iuna/style.css; \
    cp /src/iuna-work/src-tauri/icons/32x32.png /site/git/logo.png; \
    cp /src/iuna-work/src-tauri/icons/32x32.png /site/git/iuna/logo.png; \
    cp -a /src/iuna.git /site/git/iuna.git; \
    find /site/git -type f -name '*.html' -exec sed -i -E 's|<a href="(\.\./)+"><img |<a href="/"><img |g' {} +

COPY www /site
COPY downloads /site/downloads
RUN set -eux; \
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' /src/iuna-work/Cargo.toml | head -n 1)"; \
    mkdir -p /site/downloads; \
    cp /site/downloads.html /site/downloads/index.html; \
    sed -i "s|\${IUNA_VERSION}|${version}|g" /site/downloads/index.html; \
    printf '{"tag":"v%s","version":"%s","url":"https://iuna.jhx.app/downloads/"}\n' "$version" "$version" > /site/downloads/latest.json; \
    rm -f /site/downloads.html

########################################################################
# 3. Ship it - plain nginx, static files only.
########################################################################
FROM nginx:1.27-alpine

COPY --from=site-builder /site /usr/share/nginx/html
COPY nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80
