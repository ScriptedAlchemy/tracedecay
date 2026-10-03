FROM node:22-bookworm AS node
FROM ubuntu:24.04
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git python3 build-essential \
       pkg-config libssl-dev clang cmake mold gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/* \
    && ln -sf /usr/bin/mold /usr/bin/ld
ENV CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.97.1 \
       --component clippy,rustfmt,rust-analyzer,rust-src \
    && rustup target add x86_64-pc-windows-gnu \
    && chmod -R a+rX /opt/rustup /opt/cargo \
    && rm /tmp/rustup.sh
RUN npm install --global pnpm@12.6.0 cargo-hauler@0.14.0 @ast-grep/cli@0.44.0 \
    && case "$(uname -m)" in x86_64) arch=x86_64 ;; aarch64) arch=aarch64 ;; *) exit 1 ;; esac \
    && file="cargo-nextest-0.9.146-${arch}-unknown-linux-gnu.tar.gz" \
    && url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.146" \
    && curl -fsSL "$url/$file" -o /tmp/nextest.tar.gz \
    && curl -fsSL "$url/${file%.tar.gz}.sha256" -o /tmp/nextest.sha256 \
    && awk '{ print $1 "  /tmp/nextest.tar.gz" }' /tmp/nextest.sha256 | sha256sum --check \
    && tar -xzf /tmp/nextest.tar.gz -C /usr/local/bin cargo-nextest \
    && rm /tmp/nextest.*
