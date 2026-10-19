# as4tainer all-in-one image: the Rust core plus a JRE running Jariko.
# Build from the repository root:  docker build -f docker/Dockerfile -t as4tainer .

FROM rust:1-bookworm AS core
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY samples samples
RUN cargo build --release --locked

FROM gradle:8.14-jdk17 AS jvm
WORKDIR /src/jvm
COPY jvm .
RUN gradle --no-daemon fatJar

FROM eclipse-temurin:17-jre
RUN apt-get update && apt-get install -y --no-install-recommends bash diffutils && rm -rf /var/lib/apt/lists/*
COPY --from=core /src/target/release/as4tainer /usr/local/bin/as4tainer
COPY --from=jvm /src/jvm/build/libs/as4tainer-jvm.jar /opt/as4tainer/as4tainer-jvm.jar
COPY samples /opt/as4tainer/samples
COPY scripts/demo.sh scripts/samples.sh /opt/as4tainer/
COPY docker/entrypoint.sh /opt/as4tainer/entrypoint.sh
ENV AS4TAINER_RUNNER="java -jar /opt/as4tainer/as4tainer-jvm.jar" \
    AS4TAINER_PGM_PATH=/opt/as4tainer/samples/qrpglesrc \
    AS4TAINER_SAMPLES=/opt/as4tainer/samples \
    AS4TAINER_WEB_LISTEN=0.0.0.0:8080
# 5250 display sessions (telnet). Map it to a free host port, e.g. 2323.
EXPOSE 23
# The browser 5250 terminal (HTTP). Map it to a free host port, e.g. 5250.
EXPOSE 8080
ENTRYPOINT ["/opt/as4tainer/entrypoint.sh"]
