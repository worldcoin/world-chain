FROM ghcr.io/paradigmxyz/op-reth:latest

ENV DEBIAN_FRONTEND=noninteractive

# Install dependencies including CA certs and unzip for AWS CLI v2
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        unzip \
        curl \
        lz4 \
        wget \
        jq \
        tzdata && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Install AWS CLI v2
RUN curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "/tmp/awscliv2.zip" && \
    unzip /tmp/awscliv2.zip -d /tmp && \
    /tmp/aws/install && \
    rm -rf /tmp/aws /tmp/awscliv2.zip

COPY snapshot.sh /snapshot.sh
RUN chmod +x /snapshot.sh

ENTRYPOINT ["/snapshot.sh"]
