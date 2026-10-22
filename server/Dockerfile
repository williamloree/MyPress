# À construire depuis la racine du dépôt : docker build -f server/Dockerfile -t mypress-server .
FROM rust:1.94-slim AS build
WORKDIR /src
COPY . .
# Le client Tauri n'est pas nécessaire pour construire le serveur.
RUN sed -i 's|, "app/src-tauri"||' Cargo.toml && cargo build --release -p mypress-server

FROM debian:bookworm-slim
COPY --from=build /src/target/release/mypress-server /usr/local/bin/
ENV MYPRESS_BIND=0.0.0.0:8787 MYPRESS_DB=/data/mypress.db
VOLUME /data
EXPOSE 8787
CMD ["mypress-server"]
