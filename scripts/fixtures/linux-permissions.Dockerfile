FROM rust:1.90-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends bubblewrap openssh-server \
    && rm -rf /var/lib/apt/lists/* && mkdir -p /run/sshd
WORKDIR /fixture
COPY . .
# Prune the copied lock to this isolated subset (never alter the repository lock).
RUN cargo test -p kageha-sandbox -p kageha-mcp -p kageha-workspace --no-run
CMD ["cargo", "test", "--locked", "-p", "kageha-sandbox", "-p", "kageha-mcp", "-p", "kageha-workspace"]
