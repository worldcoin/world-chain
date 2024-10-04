FROM ethereum/client-go:v1.14.8

RUN apk add --no-cache jq bash go gcc clang musl-dev linux-headers

# Install golang
COPY init_state.sh /entrypoint.sh

VOLUME ["/db", "/static"]

ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]