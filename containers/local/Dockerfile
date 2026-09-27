# syntax=docker/dockerfile:1
# Publication builds from a committed source archive. Deployment consumes its digest.
FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS binary-build
ARG TARGETARCH
WORKDIR /source
COPY . .
RUN --mount=type=cache,id=horde-local-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=horde-local-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=horde-local-target-${TARGETARCH},target=/source/target,sharing=locked \
    cargo build --locked --release --bin horde \
    && mkdir /out && cp target/release/horde /out/horde
FROM docker:28.3.3-cli@sha256:0135662b510037ea581d99c2e5929c5e01185139c0b86986a418bd4da0b98a44 AS docker-cli
FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git curl ripgrep python3 nodejs npm \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 horde \
    && mkdir -p /data/horde /workspace /cache/target /home/horde/.cargo \
    && chown -R horde:horde /data /workspace /cache /home/horde
ARG CODEX_VERSION=0.153.4
ARG CLAUDE_VERSION=2.1.118
RUN npm install -g @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION}
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker-cli /usr/local/libexec/docker/cli-plugins /usr/local/libexec/docker/cli-plugins
COPY --from=binary-build /out/horde /usr/local/bin/horde
COPY skills /usr/local/bin/skills
COPY containers/local/entrypoint /usr/local/bin/horde-local
RUN chmod 0755 /usr/local/bin/horde-local
ENV HOME=/home/horde XDG_CONFIG_HOME=/data/config HORDE_SUPERVISED=1 HORDE_CONCURRENCY=1
ENV CARGO_HOME=/home/horde/.cargo
USER horde
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/horde-local"]
CMD ["--data-dir", "/data/horde", "daemon"]
