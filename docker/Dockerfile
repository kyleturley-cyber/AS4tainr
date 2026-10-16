# ibmish all-in-one image: the Rust core plus a JRE running Jariko.
# Build from the repository root:  docker build -f docker/Dockerfile -t ibmish .

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
COPY --from=core /src/target/release/ibmish /usr/local/bin/ibmish
COPY --from=jvm /src/jvm/build/libs/ibmish-jvm.jar /opt/ibmish/ibmish-jvm.jar
COPY samples /opt/ibmish/samples
COPY scripts/demo.sh scripts/samples.sh /opt/ibmish/
COPY docker/entrypoint.sh /opt/ibmish/entrypoint.sh
ENV IBMISH_RUNNER="java -jar /opt/ibmish/ibmish-jvm.jar" \
    IBMISH_PGM_PATH=/opt/ibmish/samples/qrpglesrc \
    IBMISH_SAMPLES=/opt/ibmish/samples \
    IBMISH_WEB_LISTEN=0.0.0.0:8080
# 5250 display sessions (telnet). Map it to a free host port, e.g. 2323.
EXPOSE 23
# The browser 5250 terminal (HTTP). Map it to a free host port, e.g. 5250.
EXPOSE 8080
ENTRYPOINT ["/opt/ibmish/entrypoint.sh"]
