# syntax=docker/dockerfile:1
#
# `mire` as one static binary on an image with nothing else in it.
#
#   docker build -t mire:0.1.0 .
#   docker run --rm --read-only -p 127.0.0.1:8787:8787 \
#     -v "$PWD/profiles:/etc/mire/profiles:ro" mire:0.1.0
#
# The profiles are **mounted, not baked in**. They are the input to the tool, not
# part of it: an image carrying `profiles/` would ship endpoints pointing at
# somebody else's laptop, and a new endpoint to test would mean a new image.
# `/etc/mire/profiles` exists in the image so that a run without a mount starts
# cleanly with nothing to offer, rather than failing on a missing directory.

# --- the UI ------------------------------------------------------------------
#
# Built first and copied into the Rust build: `build.rs` embeds `ui/dist` at
# compile time, and without it the binary serves a placeholder page telling you
# to build the front end.
FROM node:24.13-alpine AS ui
WORKDIR /usr/local/src/mire/ui
# The lockfile alone, so `npm ci` is only re-run when dependencies actually move.
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

# --- the Rust toolchain ------------------------------------------------------
#
# Alpine, so the target is musl and the binary links statically — which is what
# lets the runtime stage be `distroless/static` rather than an image carrying a
# libc for one program.
#
# cmake/make/perl build aws-lc-rs, the cryptography behind rustls — the one
# dependency here that is not pure Rust.
#
# The apk versions are pinned, which is a trade: the build is reproducible, and
# it stops working the day Alpine drops one of these from the branch. That is a
# one-line diff and a visible failure, which beats a silent toolchain change
# under a pinned Rust version.
FROM rust:1.97.1-alpine AS chef
RUN apk add --no-cache musl-dev=1.2.6-r2 cmake=4.2.3-r0 make=4.4.1-r4 perl=5.42.2-r0 \
    && cargo install cargo-chef --locked
WORKDIR /usr/local/src/mire

# --- the dependency recipe ---------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- the build ---------------------------------------------------------------
#
# `cargo chef cook` compiles the dependencies from the recipe alone. The recipe
# normalises away the package version and everything else that is not a
# dependency, so this layer survives a version bump and every source-only change.
FROM chef AS builder
COPY --from=planner /usr/local/src/mire/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
COPY --from=ui /usr/local/src/mire/ui/dist ui/dist
# The empty profiles directory is built here for the same reason everything else
# is: the runtime image has no shell to create it with, and no writable root to
# create it in later.
RUN cargo build --release --locked \
    && mkdir -p /out/etc/mire/profiles

# --- the runtime -------------------------------------------------------------
#
# `static` rather than `base` or `cc`: the binary needs no libc, so the image is
# a certificate bundle, timezone data, `/etc/passwd` and nothing else. Nothing
# else is nothing to patch — no shell, no package manager, no CVE feed to watch.
#
# The certificates are the part that matters: `rustls-platform-verifier` reads
# the system store, so an endpoint on a public CA works out of the box. For an
# internal CA, mount the bundle and point `--ca-bundle` at it.
FROM gcr.io/distroless/static-debian13:nonroot

COPY --from=builder /usr/local/src/mire/target/release/mire /usr/local/bin/mire
COPY --from=builder --chown=65532:65532 /out/etc/mire /etc/mire

# `HOST` is the one setting that has to change in a container. The binary
# defaults to localhost — widening it is meant to be a deliberate act — and
# inside a container localhost is a network nothing else can reach, which would
# make a published port answer nothing at all. The isolation is the container's
# to provide; publish to `127.0.0.1:8787` and it is the same exposure as running
# the binary directly.
ENV HOST=0.0.0.0 \
    PORT=8787 \
    PROFILES_DIR=/etc/mire/profiles \
    LOG_FILTER=info

USER 65532:65532
EXPOSE 8787

ENTRYPOINT ["/usr/local/bin/mire"]
