FROM ethereum/client-go:v1.14.8 as builder

RUN apk add --no-cache jq bash go

# Install golang
COPY l1-geth-entrypoint.sh /entrypoint.sh
COPY l1-bn-genesis.sh /generate-genesis.sh


VOLUME ["/db", "/static"]

ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]
