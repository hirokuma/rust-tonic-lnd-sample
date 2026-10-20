# lnd-tonic-sample

## Prepare

NigiriをLNノードありで起動しておく。

```shell
nigiri start --ln
```

ローカルでLNDを起動する。アクセスできるように`ca.pem`の作成も行っている。

```shell
$ git clone lnd-tonic-sample
$ cd lnd-tonic-sample
$ ./start.sh
$ ./openchannel.sh
```

### command

```shell
docker exec lnd-regtest lncli --lnddir=/lnd --network=regtest getinfo
```

```shell
nigiri lnd getinfo
```

## Run

実行すると、こちらで立てたlnd-regtestとNigiriのlndを接続してinvoice入力待ちになる。

```shell
cargo run -- https://127.0.0.1:20009 $PWD/lnd-data/ca.pem $PWD/lnd-data/data/chain/bitcoin/regtest/admin.macaroon
```

Nigiri lndでinvoiceを作る。
reserveより大きくないと相手から送ってもらえないので1,000satsくらいにしておく。

```shell
nigiri lnd addinvoice 1000
```

LN送金が完了すると、Nigiri lndから送ってもらうinvoice(100sats)が出力されるのでコンソールから送金する。

```shell
nigiri lnd payinvoice lnbcrt1......

## Down

```shell
nigiri stop
./clean.sh
```
