FROM ubuntu:24.04
WORKDIR /app
COPY bin/rustforge-server /app/rustforge-server
EXPOSE 6379 7379
ENTRYPOINT ["/app/rustforge-server", "--resp-port", "6379", "--binary-port", "7379", "--threads", "2", "--data-dir", "/tmp/rustforge_data"]
