use std::{io::Write, str::FromStr};

use bitcoin::secp256k1;
use rand::RngCore;
use tonic_lnd::{
    Client, invoicesrpc,
    lnrpc::{self, PaymentFailureReason, invoice::InvoiceState, payment::PaymentStatus},
    routerrpc,
    tonic::{Response, Streaming},
};

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
    let address = address.into_string().expect("address is not UTF-8");

    let ca_file = args.next().expect("missing arguments: macaroon file");
    let macaroon_file = args
        .next()
        .expect("missing arguments: cert file and macaroon file");
    println!("macaroon: {:?}", macaroon_file);
    println!("ca_file: {:?}", ca_file);

    let mut client = Client::builder()
        .address(address)
        .macaroon_path(macaroon_file)
        .cert_path(ca_file)
        .build()
        .await
        .expect("failed to build client");

    // GetInfo
    get_info(&mut client).await?;

    // Input BOLT11 invoice
    println!("Input BOLT11 invoice: ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    let invoice = input.trim();
    let dec_invoice = decode_invoice(&mut client, invoice).await?;
    let dec_invoice = dec_invoice.get_ref();
    println!("  payment_hash: {}", dec_invoice.payment_hash);
    println!("    destination: {}", dec_invoice.destination);
    println!("    num_satoshis: {}", dec_invoice.num_satoshis);

    // 自分のチャネル残高が要求額以上あることの確認
    let list = list_channels(&mut client).await?;
    if list.get_ref().channels.is_empty() {
        println!("no channels. exit.");
        return Ok(());
    }
    let mut payable = false;
    for l in &list.get_ref().channels {
        println!("  chan_id={} ... {} sats", l.chan_id, l.local_balance);
        if l.local_balance >= dec_invoice.num_satoshis {
            // 届くかどうかは別として支払える額はある
            payable = true;
            break;
        }
    }
    if !payable {
        println!("no payable channel. exit.");
        return Ok(());
    }

    // LN送金
    let mut stream = send_payment(&mut client, invoice, 100).await?;
    let stream = stream.get_mut();
    wait_payment_streaming(stream).await?;
    balance(&mut client).await?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    balance(&mut client).await?;

    // invoice作成&受信
    let mut preimage = [0u8; 32];
    rand::rng().fill_bytes(&mut preimage);
    println!("pre-defined preimage: {}", hex::encode(preimage));
    let invoice = create_invoice(&mut client, 100, "lnd-tonic-sample test", Some(preimage.to_vec())).await?;
    let invoice = invoice.get_ref();
    println!("\nPayment Request: {}", invoice.payment_request);
    let mut stream = subscribe_single_invoice(&mut client, invoice.r_hash.clone()).await?;
    let stream = stream.get_mut();
    wait_invoice_streaming(stream).await?;
    balance(&mut client).await?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    balance(&mut client).await?;

    Ok(())
}

async fn get_info(client: &mut Client) -> anyhow::Result<()> {
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

    Ok(())
}

async fn decode_invoice(
    client: &mut Client,
    invoice_str: &str,
) -> anyhow::Result<Response<lnrpc::PayReq>> {
    // DecodePayReq
    // https://lightning.engineering/api-docs/api/lnd/lightning/decode-pay-req/
    let req = lnrpc::PayReqString {
        pay_req: invoice_str.to_string(),
    };
    Ok(client.lightning().decode_pay_req(req).await?)
}

async fn list_channels(
    client: &mut Client,
) -> anyhow::Result<Response<lnrpc::ListChannelsResponse>> {
    let req = lnrpc::ListChannelsRequest {
        active_only: true,
        ..Default::default()
    };
    Ok(client.lightning().list_channels(req).await?)
}

async fn balance(client: &mut Client) -> anyhow::Result<()> {
    println!("Channel balance:");
    let list = list_channels(client).await?;
    for l in &list.get_ref().channels {
        println!("  chan_id={} ... {} sats", l.chan_id, l.local_balance);
    }

    Ok(())
}

async fn send_payment(
    client: &mut Client,
    invoice_str: &str,
    fee_limit_sat: i64,
) -> anyhow::Result<Response<Streaming<lnrpc::Payment>>> {
    // SendPaymentV2
    // https://lightning.engineering/api-docs/api/lnd/router/send-payment-v2/
    // SendPaymentV2が返すのは「支払い更新のストリーム」であり、1件受け取って
    // 終わりではない。終端ステータス(SUCCEEDED/FAILED)になるまで読み続けて
    // 状態を判定する。
    // no_inflight_updates=trueにすると途中経過の更新は通知されず終端ステータスだけになる。
    let req = routerrpc::SendPaymentRequest {
        payment_request: invoice_str.to_string(),
        // fee_limit_sat が 0 (デフォルト) だと手数料ゼロの経路しか試されず
        // FAILURE_REASON_NO_ROUTE になりやすいので、上限を明示する。
        fee_limit_sat,
        ..Default::default()
    };
    println!("Start payment...");
    Ok(client.router().send_payment_v2(req).await?)
}

async fn wait_payment_streaming(stream: &mut Streaming<lnrpc::Payment>) -> anyhow::Result<()> {
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

    Ok(())
}

async fn create_invoice(
    client: &mut Client,
    amount_sat: i64,
    memo: &str,
    preimage: Option<Vec<u8>>,
) -> anyhow::Result<Response<lnrpc::AddInvoiceResponse>> {
    // AddInvoice
    // https://lightning.engineering/api-docs/api/lnd/lightning/add-invoice/
    let preimage: Vec<u8> = preimage.unwrap_or_default();
    let req = lnrpc::Invoice {
        value: amount_sat,
        memo: memo.to_string(),
        r_preimage: preimage,
        ..Default::default()
    };
    Ok(client.lightning().add_invoice(req).await?)
}

async fn subscribe_single_invoice(
    client: &mut Client,
    hash: Vec<u8>,
) -> anyhow::Result<Response<Streaming<lnrpc::Invoice>>> {
    let req = invoicesrpc::SubscribeSingleInvoiceRequest { r_hash: hash };
    Ok(client.invoices().subscribe_single_invoice(req).await?)
}

async fn wait_invoice_streaming(stream: &mut Streaming<lnrpc::Invoice>) -> anyhow::Result<()> {
    while let Some(invoice) = stream.message().await? {
        let state = InvoiceState::try_from(invoice.state).unwrap_or_default();
        println!("  state={:?}", state);
        match state {
            InvoiceState::Settled => {
                println!("Settled: payment_hash={}", hex::encode(invoice.r_hash));
                break;
            }
            InvoiceState::Canceled => {
                println!("Cancelled");
                break;
            }
            _ => continue,
        }
    }

    Ok(())
}
