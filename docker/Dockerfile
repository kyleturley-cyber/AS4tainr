# OpenEye all-in-one image: the Rust core plus a JRE running Jariko.
# Build from the repository root:  docker build -f docker/Dockerfile -t openeye .

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
COPY --from=core /src/target/release/openeye /usr/local/bin/openeye
COPY --from=jvm /src/jvm/build/libs/openeye-jvm.jar /opt/openeye/openeye-jvm.jar
COPY samples /opt/openeye/samples
COPY scripts/demo.sh /opt/openeye/demo.sh
ENV OPENEYE_RUNNER="java -jar /opt/openeye/openeye-jvm.jar" \
    OPENEYE_PGM_PATH=/opt/openeye/samples/qrpglesrc \
    OPENEYE_SAMPLES=/opt/openeye/samples
ENTRYPOINT ["openeye"]
CMD ["--help"]
