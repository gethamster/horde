# Build from the repository root after placing Linux binaries in dist/<arch>/horde.
FROM ubuntu:24.04@sha256:008173c23f95b170204355c12626cb5a965d779a7e1283b09e9cffbb1bf33ca3
LABEL org.opencontainers.image.description="Experimental Horde runner for Google AX"
LABEL io.horde.ax.revision="d8ed0fe38bceb7842d3c47817d53d16ccdfcb601"
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git openssh-client procps python3 ripgrep tini curl nodejs npm docker.io docker-compose-v2 \
    && rm -rf /var/lib/apt/lists/*
ARG INSTALL_HARNESSES=true
ARG CODEX_VERSION=0.153.4
ARG CLAUDE_VERSION=2.1.118
RUN if [ "$INSTALL_HARNESSES" = true ]; then \
      npm install -g @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION}; \
    fi
ARG TARGETARCH
COPY dist/${TARGETARCH}/horde /usr/local/bin/horde
COPY skills /usr/local/bin/skills
COPY scripts/ax_runner.py /usr/local/libexec/horde-ax-runner.py
COPY containers/ax/ax-task-runner /usr/local/bin/ax-task-runner
RUN chmod 0755 /usr/local/bin/ax-task-runner /usr/local/libexec/horde-ax-runner.py
ENV HOME=/workspace/.horde/home
ENV XDG_CONFIG_HOME=/workspace/.horde/config
ENV XDG_CACHE_HOME=/workspace/.horde/cache
ENV HORDE_SUPERVISED=1
ENV HORDE_ISOLATION=ax-gvisor
EXPOSE 80
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/ax-task-runner"]
