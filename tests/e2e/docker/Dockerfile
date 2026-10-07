FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends git expect openssh-client python3 libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY shims /shims
COPY init.exp fixtures.txt /fleet/
RUN chmod +x /shims/pm /shims/log \
    && for n in brew npm pnpm bun gem ruby uv curl; do ln -s pm /shims/$n; done \
    && for n in notify-send systemctl launchctl osascript; do ln -s log /shims/$n; done \
    && git config --system user.name fleet && git config --system user.email fleet@example.invalid
ENV PATH=/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/root TERM=xterm-256color
WORKDIR /root
