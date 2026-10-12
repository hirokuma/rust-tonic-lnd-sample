#!/bin/bash
# * LNDのtls.certはCA証明書を含んでいるので分けてつくるためのスクリプト
# * LNDは起動時にtls.*を読み込んで期限切れであれば作り直したりするので注意が必要
#   * ファイルと作り直してもLNDを再起動しないと反映されない
# * ca.key/ca.pemを作り直したらtls.certも作り直す。tls.keyは作り直さなくて良い。
# * tls.keyを作り直したらtls.certも作り直す
# * server.csrはtls.keyから作ることができる(スクリプトの最後で削除している)
# * tls.keyを作り直してもそれほど影響は少ない
# * ca.key/ca.pemの期限が切れると作り直しにするので期限を長めにするほうが運用しやすい

mkdir -p lnd-data
cd lnd-data

# dockerコンテナ名など。複数の場合は"DNS:abc1,DNS:abc2"のように書く。
ADD_DNS="DNS:lnd-regtest"

# ca.pemの期限が切れたら全部作り直し
CA_PEM_DAYS=3650

# tls.certの期限切れならそれだけ作り直す
TLS_CERT_DAYS=365

if [ -f tls.cert ]; then
    echo "'tls.cert' already exists."
    exit 1
fi
if ([ -f ca.key ] && [ ! -f ca.pem ]) || ([ ! -f ca.key ] && [ -f ca.pem ]); then
    echo "'ca.key' or 'ca.pem' exists."
    exit 1
fi

if [ -n "$ADD_DNS" ]; then
    ADD_DNS="$ADD_DNS,"
fi

# CA(自己署名、CA:TRUE)
if [ ! -f ca.key ] && [ ! -f ca.pem ]; then
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -nodes -days $CA_PEM_DAYS -keyout ca.key -out ca.pem \
        -subj "/O=lnd-regtest-ca" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign"
    echo "Create 'ca.key' and 'ca.pem'."
else
    echo "Use existing 'ca.key' and 'ca.pem'."
fi

# サーバ証明書(CA:FALSE)のCSR
if [ ! -f tls.key ]; then
    # tls.keyを作り直すならtls.certも作り直すが、ca.key/ca.pemの作り直しとtls.keyは関係ない
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -nodes -keyout tls.key -out server.csr \
        -subj "/O=lnd"
    chmod 600 tls.key
    echo "Create 'tls.key"
elif [ ! -f server.csr ]; then
    # server.csrをtls.keyから作る
    openssl req -new -key tls.key -out server.csr -subj "/O=lnd"
    echo "Use existing 'tls.key'"
fi

# CAで署名
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
  -days $TLS_CERT_DAYS -out tls.cert \
  -extfile <(printf "subjectAltName=${ADD_DNS}DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth") > /dev/null 2>&1
rm -f server.csr ca.srl

echo
echo "'tls.cert' created."
echo
sha256sum ca.key
sha256sum ca.pem
sha256sum tls.key
sha256sum tls.cert
