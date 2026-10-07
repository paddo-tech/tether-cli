# A machine for the e2e suite. Package managers, curl and desktop tools are shims that log
# each call to /state/calls.jsonl, so nothing installs and no package request leaves the box.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends tini git expect openssh-client python3 libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY shims /shims
COPY init.exp /e2e/
RUN chmod +x /shims/pm /shims/log \
    && for n in brew npm pnpm bun gem ruby uv curl; do ln -s pm /shims/$n; done \
    && for n in notify-send systemctl launchctl osascript; do ln -s log /shims/$n; done \
    && git config --system user.name e2e && git config --system user.email e2e@example.invalid \
    && mkdir -p /state/pkgs && echo init > /state/round
ENV PATH=/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/root TERM=xterm-256color
WORKDIR /root
# tini reaps processes that exit after their parent, such as a stopped daemon
ENTRYPOINT ["/usr/bin/tini", "--"]
