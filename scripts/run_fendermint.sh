#!/bin/sh
set -eu

rm -rf ~/.fendermint/data/rocksdb
FM_VALIDATOR_KEY__PATH=keys/validator.sk FM_VALIDATOR_KEY__KIND=regular FM_NETWORK=test FM_RESOLVER__CONNECTION__LISTEN_ADDR=/ip4/127.0.0.1/tcp/3001 fendermint run
