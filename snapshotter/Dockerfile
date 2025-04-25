FROM ghcr.io/paradigmxyz/op-reth:latest

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        lz4 \
        awscli \
        curl \
        wget \
        jq \
        tzdata && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY snapshot.sh /snapshot.sh
RUN chmod +x /snapshot.sh

ENTRYPOINT ["/snapshot.sh"]
