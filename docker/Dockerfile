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

# Pinned by version and by digest (security review 2026-09-13). `rust:1` moved to every new
# stable, so the image built today and the one built next month ran different compilers, and a
# tag can be repointed; a digest cannot. The local gates run on this toolchain, so bumping it is
# the same deliberate change as bumping clippy's pin in the workflow. To bump: pick the version,
# then `docker buildx imagetools inspect rust:<version>-slim-bookworm` for the index digest.
FROM rust:1.98.1-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730

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
#
# Every piece pinned (security review 2026-09-13). This used to pipe cargo-binstall's install
# script from its `main` branch into bash and install whatever cargo-deny and cargo-audit were
# newest, so the gates that judge the dependencies were themselves unpinned. cargo-binstall is
# now a release tarball checked against the SHA-256 recorded here, and the two tools are named
# by version. The hashes were taken from the v1.24.0 release assets on 2026-09-26.
ARG BINSTALL_VERSION=1.24.0
ARG BINSTALL_SHA256_X86_64=7ffe1832c4bc3148d00f02d51c1d96c9cccdec76a0fa20e6e5692201d78bcc0b
ARG BINSTALL_SHA256_AARCH64=c2c44aba65d402ac43ee821387c49246bc574bde65d3d7f0f7a9e666c5a69f7f
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_AUDIT_VERSION=0.22.2
RUN set -eu; \
    arch="$(uname -m)"; \
    case "${arch}" in \
        x86_64) sha="${BINSTALL_SHA256_X86_64}" ;; \
        aarch64) sha="${BINSTALL_SHA256_AARCH64}" ;; \
        *) echo "no pinned cargo-binstall for ${arch}" >&2; exit 1 ;; \
    esac; \
    tarball="cargo-binstall-${arch}-unknown-linux-musl.tgz"; \
    curl --proto '=https' --tlsv1.2 -sSfL -o "/tmp/${tarball}" \
        "https://github.com/cargo-bins/cargo-binstall/releases/download/v${BINSTALL_VERSION}/${tarball}"; \
    echo "${sha}  /tmp/${tarball}" | sha256sum -c -; \
    tar -xzf "/tmp/${tarball}" -C "${CARGO_HOME}/bin" cargo-binstall; \
    rm "/tmp/${tarball}"; \
    cargo binstall --no-confirm --locked \
        "cargo-deny@${CARGO_DENY_VERSION}" "cargo-audit@${CARGO_AUDIT_VERSION}"; \
    rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

# Out of the repo, so the container never writes into the mounted working tree.
ENV CARGO_TARGET_DIR=/build/target \
    CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0

WORKDIR /repo
COPY docker/ci.sh /usr/local/bin/splitforge-ci
RUN chmod +x /usr/local/bin/splitforge-ci

ENTRYPOINT ["/usr/local/bin/splitforge-ci"]
CMD ["all"]
