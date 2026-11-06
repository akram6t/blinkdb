FROM ubuntu:24.04

LABEL org.opencontainers.image.title="BlinkDB"
LABEL org.opencontainers.image.description="A Linux-Native, Thread-Per-Core, Tiered In-Memory Database in Rust"
LABEL org.opencontainers.image.source="https://github.com/akram6t/blinkdb"
LABEL org.opencontainers.image.url="https://github.com/akram6t/blinkdb"
LABEL org.opencontainers.image.documentation="https://github.com/akram6t/blinkdb/blob/main/README.md"
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

WORKDIR /app
COPY README.md /app/README.md
COPY bin/blinkdb-server /app/blinkdb-server
RUN ln -s /app/blinkdb-server /app/rustforge-server

EXPOSE 6379 7379
ENTRYPOINT ["/app/blinkdb-server"]
CMD ["--resp-port", "6379", "--binary-port", "7379", "--threads", "2", "--data-dir", "/tmp/blinkdb_data"]
