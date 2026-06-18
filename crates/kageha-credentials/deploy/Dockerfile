# Build from repository root: docker build -f crates/kageha-credentials/deploy/Dockerfile .
FROM rust:1.90-bookworm AS build
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY xtask ./xtask
RUN cargo build --locked --release -p kageha-credentials --features service,client
FROM debian:bookworm-slim
RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin credentials
COPY --from=build /source/target/release/kageha-credentials /usr/local/bin/kageha-credentials
USER 10001:10001
EXPOSE 8443
ENTRYPOINT ["kageha-credentials"]
CMD ["serve", "--config", "/run/config/credentials.json"]
