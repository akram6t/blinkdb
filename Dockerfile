FROM ubuntu:24.04
WORKDIR /app
COPY bin/blinkdb-server /app/blinkdb-server
RUN ln -s /app/blinkdb-server /app/rustforge-server
EXPOSE 6379 7379
ENTRYPOINT ["/app/blinkdb-server"]
CMD ["--resp-port", "6379", "--binary-port", "7379", "--threads", "2", "--data-dir", "/tmp/blinkdb_data"]
