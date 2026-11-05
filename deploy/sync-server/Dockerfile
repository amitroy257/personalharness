# Personal Harness sync server. Build from the repository root:
#   docker build -f deploy/sync-server/Dockerfile -t harness-sync .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p harness-sync-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 harness && mkdir /data && chown harness /data
COPY --from=build /src/target/release/harness-sync /usr/local/bin/harness-sync
USER harness
ENV ADDR=0.0.0.0:8787 DATA_DIR=/data
EXPOSE 8787
VOLUME /data
CMD ["harness-sync"]
