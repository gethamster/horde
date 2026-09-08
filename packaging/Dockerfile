FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git ripgrep curl nodejs npm && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 horde && mkdir /data && chown horde:horde /data
ARG CODEX_VERSION=0.153.4
ARG CLAUDE_VERSION=2.1.118
RUN npm install -g @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION}
ARG TARGETARCH
COPY dist/${TARGETARCH}/horde /usr/local/bin/horde
USER horde
ENV HOME=/home/horde
ENV XDG_CONFIG_HOME=/data/config
ENV HORDE_SUPERVISED=1
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/horde"]
CMD ["--data-dir", "/data", "daemon"]
