# pango-groupware 실행 이미지.
#
#   docker build -t pango-groupware .
#   docker run --rm -v pango-data:/data pango-groupware init --company "우리 회사"
#   docker run -d -p 8080:8080 -v pango-data:/data pango-groupware
#
# 자세한 운영 방법은 docs/DEPLOY.md.

FROM rust:1.92-bookworm AS build
RUN rustup target add wasm32-unknown-unknown \
 && cargo install wasm-bindgen-cli --version 0.2.130 --locked
WORKDIR /src
COPY . .
# 웹 UI 번들을 먼저 만들어야 서버 실행 파일에 들어간다.
RUN cargo xtask web \
 && cargo build --release --locked -p pango-server \
 && cp target/release/pango-groupware /pango-groupware

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin pango \
 && mkdir -p /data && chown pango:pango /data
COPY --from=build /pango-groupware /usr/local/bin/pango-groupware
USER pango
ENV PANGO_DATA_DIR=/data \
    PANGO_BIND=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["pango-groupware"]
CMD ["serve"]
