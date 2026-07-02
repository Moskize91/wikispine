FROM rust:1-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p wikispine-runtime --bin wikispine

FROM debian:bookworm-slim

RUN useradd --create-home --shell /usr/sbin/nologin wikispine
COPY --from=builder /app/target/release/wikispine /usr/local/bin/wikispine

ENV WIKISPINE_DATA_DIR=/data/runtime
ENV PORT=9000

USER wikispine
WORKDIR /home/wikispine
EXPOSE 9000

ENTRYPOINT ["wikispine"]
CMD ["serve"]
