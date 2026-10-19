# as4tainr all-in-one image: one Rust binary (the core, the 5250 server,
# the browser terminal and the RPG engine), no JVM.
# Build from the repository root:  docker build -f docker/Dockerfile -t as4tainr .

FROM rust:1-bookworm AS core
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY samples samples
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends bash diffutils ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=core /src/target/release/as4tainr /usr/local/bin/as4tainr
COPY samples /opt/as4tainr/samples
COPY scripts/demo.sh scripts/samples.sh /opt/as4tainr/
COPY docker/entrypoint.sh /opt/as4tainr/entrypoint.sh
ENV AS4TAINR_PGM_PATH=/opt/as4tainr/samples/qrpglesrc \
    AS4TAINR_SAMPLES=/opt/as4tainr/samples \
    AS4TAINR_WEB_LISTEN=0.0.0.0:8080
# 5250 display sessions (telnet). Map it to a free host port, e.g. 2323.
EXPOSE 23
# The browser 5250 terminal (HTTP). Map it to a free host port, e.g. 5250.
EXPOSE 8080
ENTRYPOINT ["/opt/as4tainr/entrypoint.sh"]
