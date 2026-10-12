use std::{io::Write, str::FromStr};

use bitcoin::secp256k1;
use tonic_lnd::lnrpc::{PaymentFailureReason, payment::PaymentStatus};
use tonic_lnd::{Client, lnrpc, routerrpc};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os();
    if args.len() != 4 {
        eprintln!("Usage: cargo run -- <URL> <CA_file> <macaroon_file>");
        eprintln!(
            "\n  ex.: cargo run -- https://127.0.0.1:20009 $PWD/lnd-data/ca.pem $PWD/lnd-data/data/chain/bitcoin/regtest/admin.macaroon"
        );
        return Ok(());
    }
    args.next().expect("not even zeroth arg given");

    let address = args
        .next()
        .expect("missing arguments: address, cert file, macaroon file");
    let ca_file = args.next().expect("missing arguments: macaroon file");
    let macaroon_file = args
        .next()
        .expect("missing arguments: cert file and macaroon file");
    println!("macaroon: {:?}", macaroon_file);
    println!("ca_file: {:?}", ca_file);

    let address = address.into_string().expect("address is not UTF-8");
    let mut client = Client::builder()
        .address(address)
        .macaroon_path(macaroon_file)
        .cert_path(ca_file)
        .build()
        .await
        .expect("failed to build client");

    // GetInfo
    // https://lightning.engineering/api-docs/api/lnd/lightning/get-info/
    let info = client
        .lightning()
        .get_info(lnrpc::GetInfoRequest {})
        .await
        .expect("failed to get info");
    println!("{info:#?}");
    let node_id = secp256k1::PublicKey::from_str(&info.get_ref().identity_pubkey)?;
    println!("node_id={}", node_id);

    // Input BOLT11 invoice
    println!("Input BOLT11 invoice: ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    let invoice = input.trim();

    // DecodePayReq
    // https://lightning.engineering/api-docs/api/lnd/lightning/decode-pay-req/
    let req = lnrpc::PayReqString {
        pay_req: invoice.to_string(),
    };
    let dec_invoice = client.lightning().decode_pay_req(req).await?;
    let dec_invoice = dec_invoice.get_ref();
    println!("  destination: {}", dec_invoice.destination);
    println!("  num_satoshis: {}", dec_invoice.num_satoshis);

    // ListChannels
    // https://lightning.engineering/api-docs/api/lnd/lightning/list-channels/
    let req = lnrpc::ListChannelsRequest {
        active_only: true,
        ..Default::default()
    };
    let list = client.lightning().list_channels(req).await?;
    if list.get_ref().channels.is_empty() {
        println!("no channels. exit.");
        return Ok(());
    }
    let mut payable = false;
    for l in &list.get_ref().channels {
        println!("  chan_id={} ... {} sats", l.chan_id, l.local_balance);
        if l.local_balance > dec_invoice.num_satoshis {
            // 届くかどうかは別として支払える額はある
            payable = true;
            break;
        }
    }
    if !payable {
        println!("no payable channel. exit.");
        return Ok(());
    }

    // SendPaymentV2
    // https://lightning.engineering/api-docs/api/lnd/router/send-payment-v2/
    // SendPaymentV2が返すのは「支払い更新のストリーム」であり、1件受け取って
    // 終わりではない。終端ステータス(SUCCEEDED/FAILED)になるまで読み続けて
    // 状態を判定する。
    // no_inflight_updates=trueにすると途中経過の更新は通知されず終端ステータスだけになる。
    let req = routerrpc::SendPaymentRequest {
        payment_request: invoice.to_string(),
        // fee_limit_sat が 0 (デフォルト) だと手数料ゼロの経路しか試されず
        // FAILURE_REASON_NO_ROUTE になりやすいので、上限を明示する。
        fee_limit_sat: 100,
        ..Default::default()
    };
    println!("Start payment...");
    let mut stream = client.router().send_payment_v2(req).await?.into_inner();

    // 通知があるまで待機するタイプ
    while let Some(payment) = stream.message().await? {
        // status は i32 なので列挙型に変換する。
        let status = PaymentStatus::try_from(payment.status).unwrap_or_default();
        println!(
            "  status={:?}, value={} sats, fee={} sats",
            status, payment.value_sat, payment.fee_sat
        );

        match status {
            // 途中経過 (HTLC飛行中 or まだ試行していない) は読み進める。
            PaymentStatus::InFlight | PaymentStatus::Initiated => continue,
            PaymentStatus::Succeeded => {
                println!("payment succeeded: preimage={}", payment.payment_preimage);
                break;
            }
            PaymentStatus::Failed => {
                let reason =
                    PaymentFailureReason::try_from(payment.failure_reason).unwrap_or_default();
                println!("payment failed: reason={:?}", reason);
                break;
            }
            // Unknown は「返らない」想定だが、念のため継続。
            _ => continue,
        }
    }

    // チャネル残高を取得
    let req = lnrpc::ListChannelsRequest {
        active_only: true,
        ..Default::default()
    };
    let list = client.lightning().list_channels(req).await?;
    for l in &list.get_ref().channels {
        println!("  chan_id={} ... {} sats", l.chan_id, l.local_balance);
    }

    Ok(())
}
