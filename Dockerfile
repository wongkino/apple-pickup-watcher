# 网页版：先编出 React 界面，再编 apw-web。桌面版的托盘和系统通知不进镜像。
# chrome-tls（btls-sys）编译需要 cmake、C++ 和 libclang；提示音库在 Linux 上还要 ALSA 头文件。

FROM node:22-bookworm-slim AS web

RUN corepack enable && corepack prepare pnpm@9.15.9 --activate
WORKDIR /src
COPY package.json pnpm-lock.yaml ./
COPY index.html vite.config.ts tsconfig.json tsconfig.node.json ./
COPY src ./src
RUN pnpm install --frozen-lockfile && pnpm exec vite build

FROM rust:1.98-bookworm AS build

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        cmake \
        g++ \
        libasound2-dev \
        libclang-dev \
        make \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri/Cargo.toml ./src-tauri/Cargo.toml
COPY src-tauri/src ./src-tauri/src

ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8

RUN cargo build --release --locked -p apw-web

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libasound2 \
        python3 \
        python3-venv \
    && python3 -m venv /opt/apprise \
    && /opt/apprise/bin/pip install --no-cache-dir apprise \
    && ln -s /opt/apprise/bin/apprise /usr/local/bin/apprise \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 --user-group apw

COPY --from=build /src/target/release/apw-web /usr/local/bin/apw-web
COPY --from=web /src/dist /usr/local/share/apw-web
COPY docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 755 /usr/local/bin/apw-web /usr/local/bin/docker-entrypoint.sh
ENV APW_WEB_DIST=/usr/local/share/apw-web \
    APW_WEB_LISTEN=0.0.0.0:8080

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
