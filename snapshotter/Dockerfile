FROM ghcr.io/worldcoin/world-chain:v1.9.3

ENV DEBIAN_FRONTEND=noninteractive

COPY snapshot.sh /snapshot.sh
RUN chmod +x /snapshot.sh

ENTRYPOINT ["/snapshot.sh"]
