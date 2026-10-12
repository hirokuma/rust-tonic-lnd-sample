#!/bin/bash

function lnd() {
    docker exec lnd-regtest lncli --lnddir=/lnd --network=regtest $@
}

function nigiri_lnd() {
    echo -n $(nigiri lnd $@)
}

LOCAL_LN=$(lnd getinfo | jq -r .identity_pubkey)
NIGIRI_LN=$(nigiri_lnd getinfo | jq -r .identity_pubkey)

echo "lnd-regtest: $LOCAL_LN"
echo "nigiri-ln  : $NIGIRI_LN"

LOCAL_BTC=$(lnd newaddress p2tr | jq -r .address)
nigiri faucet $LOCAL_BTC

nigiri lnd connect $LOCAL_LN@lnd-regtest:19735
lnd connect $NIGIRI_LN@lnd:9735

lnd openchannel $NIGIRI_LN 20000
nigiri rpc generatetoaddress 3 $LOCAL_BTC
