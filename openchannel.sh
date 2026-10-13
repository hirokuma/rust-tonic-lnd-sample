#!/bin/bash -e

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

PEERS=$(nigiri lnd listpeers | jq '.peers | length')
if [ $PEERS -eq 0 ]; then
    nigiri lnd connect $LOCAL_LN@lnd-regtest:9735
    # lnd connect $NIGIRI_LN@lnd:9735
fi

CHANS=$(nigiri lnd listchannels | jq '.channels | length')
if [ $CHANS -ne 0 ]; then
    echo "Has LN channels."
    exit 0
fi

LOCAL_BTC=$(lnd newaddress p2tr | jq -r .address)
nigiri faucet $LOCAL_BTC

sleep 1

lnd openchannel $NIGIRI_LN 20000
nigiri rpc generatetoaddress 3 $LOCAL_BTC
while :
do
    CHANS=$(nigiri lnd listchannels | jq '.channels | length')
    if [ $CHANS -ne 0 ]; then
        break
    fi
done
echo "channel created."
