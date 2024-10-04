FROM ethereum/client-go:v1.14.8

RUN apk add --no-cache jq bash go gcc clang musl-dev linux-headers

# Install golang
COPY l1-geth-entrypoint.sh /entrypoint.sh

VOLUME ["/db", "/static"]

ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]
