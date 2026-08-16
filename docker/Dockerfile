# The CI environment, reproducible locally.
#
# Exists for one gate in particular: the Raspberry Pi cross-build needs
# `gcc-aarch64-linux-gnu`, because `rusqlite` is built with the `bundled` feature
# (ADR-0009) and so compiles SQLite itself from C source for the target. That toolchain is
# a Linux package, which means a developer on Windows or macOS cannot run the gate that is
# most likely to catch a dependency they just added.
#
# `docs/ci.md` claims the local commands are identical to CI's. This is what makes that
# true on a machine that is not Ubuntu.
#
#   docker build -t splitforge-ci -f docker/Dockerfile .
#   docker run --rm -v "$PWD:/repo" -v splitforge-target:/build splitforge-ci
#
# The target directory is a named volume rather than the repo's `target/`, so a Linux build
# never fights the host's artifacts over the same directory.

FROM rust:1-slim-bookworm

# build-essential: build scripts compile C on the host side too (bundled SQLite for the
#   native target, ring, etc.).
# gcc-aarch64-linux-gnu: the cross compiler and linker named in .cargo/config.toml.
# libc6-dev-arm64-cross: the target's libc headers and startup objects. Listed explicitly
#   because `--no-install-recommends` drops it — without it `gcc-aarch64-linux-gnu` compiles
#   nothing that includes <stdint.h> and links nothing at all (`cannot find Scrt1.o`).
#   CI installs with recommends enabled and so gets it implicitly; being explicit here means
#   the image does not depend on that happening to stay true.
# ca-certificates + curl: fetching cargo-binstall and the advisory database.
# git: cargo-audit clones the RustSec advisory database.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        gcc-aarch64-linux-gnu \
        git \
        libc6-dev-arm64-cross \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Both toolchains CI uses: whatever stable is, plus the MSRV floor from ADR-0013.
# Pinned here rather than resolved at run time so the image is reproducible.
ARG MSRV=1.88.0
RUN rustup component add clippy rustfmt \
    && rustup target add aarch64-unknown-linux-gnu \
    && rustup toolchain install "${MSRV}" --profile minimal \
    && rustup target add aarch64-unknown-linux-gnu --toolchain "${MSRV}"

# cargo-deny and cargo-audit as prebuilt binaries. Building them from source costs several
# minutes and pulls in a dependency tree larger than the project's own.
RUN curl --proto '=https' --tlsv1.2 -sSfL \
        https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh \
        | bash \
    && cargo binstall --no-confirm cargo-deny cargo-audit \
    && rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

# Out of the repo, so the container never writes into the mounted working tree.
ENV CARGO_TARGET_DIR=/build/target \
    CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0

WORKDIR /repo
COPY docker/ci.sh /usr/local/bin/splitforge-ci
RUN chmod +x /usr/local/bin/splitforge-ci

ENTRYPOINT ["/usr/local/bin/splitforge-ci"]
CMD ["all"]
