FROM rust:1-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p wikispine-runtime

FROM debian:bookworm-slim

RUN useradd --create-home --shell /usr/sbin/nologin wikispine
COPY --from=builder /app/target/release/wikispine-runtime /usr/local/bin/wikispine-runtime

USER wikispine
WORKDIR /home/wikispine
EXPOSE 8719

ENTRYPOINT ["wikispine-runtime"]
CMD ["serve", "--dataset", "/data/runtime", "--bind", "0.0.0.0:8719"]
