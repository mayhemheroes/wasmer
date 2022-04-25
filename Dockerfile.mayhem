## Use Rust to build
FROM rustlang/rust:nightly as builder

## Add source code to the build stage.
ADD . /wasmer
WORKDIR /wasmer

RUN cargo install cargo-fuzz

## TODO: ADD YOUR BUILD INSTRUCTIONS HERE.
WORKDIR /wasmer/fuzz
RUN cargo +nightly fuzz build --features "dylib cranelift" dylib_cranelift
# Output binary is placed in /wasmer/target/x86_64-unknown-linux-gnu/release/dylib_cranelift

# Package Stage
FROM --platform=linux/amd64 ubuntu:20.04

## Install build dependencies.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y gcc

## Copy the binary from the build stage to an Ubuntu docker image
COPY --from=builder /wasmer/target/x86_64-unknown-linux-gnu/release/dylib_cranelift /
