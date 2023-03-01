FROM clux/muslrust:stable as build-env
VOLUME /root/.cargo
COPY . /volume
RUN cargo build --release

FROM gcr.io/distroless/cc
COPY --from=build-env /volume/target/x86_64-unknown-linux-musl/release/adguard-external-dns /
ENTRYPOINT ["./adguard-external-dns"]
