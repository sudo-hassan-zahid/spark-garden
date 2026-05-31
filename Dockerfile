FROM rust:1.87-slim AS builder

WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN useradd --create-home --uid 10001 appuser
WORKDIR /app
COPY --from=builder /app/target/release/spark-garden /usr/local/bin/spark-garden
RUN mkdir -p /app/data && chown -R appuser:appuser /app

USER appuser
EXPOSE 8080
ENV SPARK_ADDR=0.0.0.0:8080
ENV SPARK_DATA=/app/data/spark-garden.tsv
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 CMD ["/usr/local/bin/spark-garden", "--healthcheck"]

CMD ["spark-garden"]
