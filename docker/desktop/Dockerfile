# Build with the repository root as context.
FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY xtask ./xtask
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    cargo build --locked --release -p kageha-workspace --bin kageha-shell-service \
    && cp target/release/kageha-shell-service /kageha-shell-service

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends chromium xvfb openbox scrot xdotool xterm socat curl fonts-liberation ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home desktop
LABEL kageha.desktop.version="3"
COPY --from=build /kageha-shell-service /usr/local/bin/
USER desktop
ENV DISPLAY=:99
COPY --chmod=755 docker/desktop/entrypoint.sh /entrypoint.sh
HEALTHCHECK --interval=2s --timeout=2s --start-period=15s \
    CMD xdotool getdisplaygeometry && curl -fsS http://127.0.0.1:9222/json/version \
      && (printf '%s\n' '{"op":"ping"}' | socat -T 1 -t 1 STDIO UNIX-CONNECT:/tmp/kageha-shell.sock | grep -q '"ok":true')
ENTRYPOINT ["/entrypoint.sh"]
