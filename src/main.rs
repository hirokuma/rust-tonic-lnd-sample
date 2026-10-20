use std::{io::Write, path::PathBuf, str::FromStr};

use anyhow::{Context, Result};
use bitcoin::secp256k1;
use clap::Parser;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tonic_lnd::{
    Client, invoicesrpc,
    lnrpc::{self, PaymentFailureReason, invoice::InvoiceState, payment::PaymentStatus},
    routerrpc,
    tonic::{Response, Streaming},
};

#[derive(Debug, Parser)]
#[command(name = "lnd-tonic-sample")]
struct Args {
    #[arg(help = "LND gRPC URL, e.g. https://127.0.0.1:20009", default_value = "https://127.0.0.1:20009")]
    address: String,

    #[arg(help = "Path to the LND CA certificate", default_value = "$PWD/lnd-data/ca.pem")]
    ca_file: String,

    #[arg(help = "Path to the admin macaroon file", default_value = "$PWD/lnd-data/data/chain/bitcoin/regtest/admin.macaroon")]
    macaroon_file: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut client = Client::builder()
        .address(args.address.clone())
        .macaroon_path(decode_file_path(&args.macaroon_file))
        .cert_path(decode_file_path(&args.ca_file))
        .build()
        .await
        .with_context(|| {
            format!(
                "failed to build LND client for address={} ca_file={} macaroon_file={}",
                args.address,
                args.ca_file,
                args.macaroon_file
            )
        })?;

    get_info(&mut client).await?;

    let invoice = read_invoice_from_stdin()?;
    let dec_invoice = decode_invoice(&mut client, &invoice).await?;
    let dec_invoice = dec_invoice.get_ref();

    println!("  payment_hash: {}", dec_invoice.payment_hash);
    println!("    destination: {}", dec_invoice.destination);
    println!("    num_satoshis: {}", dec_invoice.num_satoshis);

    let list = list_channels(&mut client).await?;
    if list.get_ref().channels.is_empty() {
        println!("no channels. exit.");
        return Ok(());
    }

    let client_payment = client.clone();
    let _ = tokio::spawn(track_payments(client_payment));

    let mut payable = false;
    for channel in &list.get_ref().channels {
        println!(
            "  chan_id={} ... {} sats",
            channel.chan_id, channel.local_balance
        );
        if channel.local_balance >= dec_invoice.num_satoshis {
            payable = true;
            break;
        }
    }

    if !payable {
        println!("no payable channel. exit.");
        return Ok(());
    }

    let mut stream = send_payment(&mut client, &invoice, 100).await?;
    wait_payment_streaming(stream.get_mut()).await?;
    balance(&mut client).await?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    balance(&mut client).await?;

    let mut preimage = [0u8; 32];
    rand::rng().fill_bytes(&mut preimage);
    println!("pre-defined preimage: {}", hex::encode(preimage));

    let invoice_resp = create_invoice(
        &mut client,
        100,
        "lnd-tonic-sample test",
        Some(preimage.to_vec()),
    )
    .await?;

    let invoice = invoice_resp.get_ref();
    println!("\nPayment Request: {}", invoice.payment_request);

    let mut stream = subscribe_single_invoice(&mut client, invoice.r_hash.clone()).await?;
    wait_invoice_streaming(stream.get_mut(), false).await?;
    balance(&mut client).await?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    balance(&mut client).await?;
    println!();

    let mut preimage = [0u8; 32];
    rand::rng().fill_bytes(&mut preimage);
    println!("Hodl invoice's preimage: {}", hex::encode(preimage));

    let payment_hash = Sha256::digest(preimage).to_vec();
    let hold_resp = create_hold_invoice(
        &mut client,
        100,
        "lnd-tonic-sample test",
        payment_hash.clone(),
    )
    .await?;

    let invoice = hold_resp.get_ref();
    println!("\nPayment Request: {}", invoice.payment_request);

    let mut stream = subscribe_single_invoice(&mut client, payment_hash).await?;
    wait_invoice_streaming(stream.get_mut(), true).await?;

    settle_invoice(&mut client, preimage.to_vec()).await?;

    balance(&mut client).await?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    balance(&mut client).await?;

    Ok(())
}

fn read_invoice_from_stdin() -> Result<String> {
    print!("Input BOLT11 invoice: ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let invoice = input.trim();

    if invoice.is_empty() {
        anyhow::bail!("invoice is empty");
    }

    Ok(invoice.to_string())
}

async fn get_info(client: &mut Client) -> Result<()> {
    // GetInfo
    // https://lightning.engineering/api-docs/api/lnd/lightning/get-info/
    let info = client
        .lightning()
        .get_info(lnrpc::GetInfoRequest {})
        .await
        .context("failed to get LND info")?;
    println!("{info:#?}");

    let node_id = secp256k1::PublicKey::from_str(&info.get_ref().identity_pubkey)
        .context("failed to parse identity_pubkey as secp256k1 public key")?;
    println!("node_id={}", node_id);

    Ok(())
}

async fn decode_invoice(client: &mut Client, invoice_str: &str) -> Result<Response<lnrpc::PayReq>> {
    // DecodePayReq
    // https://lightning.engineering/api-docs/api/lnd/lightning/decode-pay-req/
    let req = lnrpc::PayReqString {
        pay_req: invoice_str.to_string(),
    };
    client
        .lightning()
        .decode_pay_req(req)
        .await
        .context("failed to decode BOLT11 invoice")
}

async fn list_channels(client: &mut Client) -> Result<Response<lnrpc::ListChannelsResponse>> {
    // ListChannels
    // https://lightning.engineering/api-docs/api/lnd/lightning/list-channels/
    let req = lnrpc::ListChannelsRequest {
        active_only: true,
        ..Default::default()
    };

    client
        .lightning()
        .list_channels(req)
        .await
        .context("failed to list channels")
}

async fn balance(client: &mut Client) -> Result<()> {
    println!("Channel balance:");
    let list = list_channels(client).await?;

    for channel in &list.get_ref().channels {
        println!(
            "  chan_id={} ... {} sats",
            channel.chan_id, channel.local_balance
        );
    }

    Ok(())
}

async fn send_payment(
    client: &mut Client,
    invoice_str: &str,
    fee_limit_sat: i64,
) -> Result<Response<Streaming<lnrpc::Payment>>> {
    // SendPaymentV2
    // https://lightning.engineering/api-docs/api/lnd/router/send-payment-v2/
    let req = routerrpc::SendPaymentRequest {
        payment_request: invoice_str.to_string(),
        fee_limit_sat,
        ..Default::default()
    };

    println!("Start payment...");
    client
        .router()
        .send_payment_v2(req)
        .await
        .context("failed to start payment stream")
}

async fn wait_payment_streaming(stream: &mut Streaming<lnrpc::Payment>) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);

    while let Some(payment) = tokio::time::timeout_at(deadline, stream.message()).await?? {
        let status = PaymentStatus::try_from(payment.status).unwrap_or_default();
        println!(
            "  send_payment: status={:?}, value={} sats, fee={} sats",
            status, payment.value_sat, payment.fee_sat
        );

        match status {
            PaymentStatus::InFlight | PaymentStatus::Initiated => continue,
            PaymentStatus::Succeeded => {
                println!("payment succeeded: preimage={}", payment.payment_preimage);
                return Ok(());
            }
            PaymentStatus::Failed => {
                let reason =
                    PaymentFailureReason::try_from(payment.failure_reason).unwrap_or_default();
                println!("payment failed: reason={:?}", reason);
                return Ok(());
            }
            _ => continue,
        }
    }

    anyhow::bail!("payment stream closed without terminal status");
}

async fn create_invoice(
    client: &mut Client,
    amount_sat: i64,
    memo: &str,
    preimage: Option<Vec<u8>>,
) -> Result<Response<lnrpc::AddInvoiceResponse>> {
    // AddInvoice
    // https://lightning.engineering/api-docs/api/lnd/lightning/add-invoice/
    let r_preimage = preimage.unwrap_or_default();
    let req = lnrpc::Invoice {
        value: amount_sat,
        memo: memo.to_string(),
        r_preimage,
        ..Default::default()
    };

    client
        .lightning()
        .add_invoice(req)
        .await
        .context("failed to create invoice")
}

async fn subscribe_single_invoice(
    client: &mut Client,
    hash: Vec<u8>,
) -> Result<Response<Streaming<lnrpc::Invoice>>> {
    // SubscribeSingleInvoice
    // https://lightning.engineering/api-docs/api/lnd/invoices/subscribe-single-invoice/
    let req = invoicesrpc::SubscribeSingleInvoiceRequest { r_hash: hash };
    client
        .invoices()
        .subscribe_single_invoice(req)
        .await
        .context("failed to subscribe to invoice updates")
}

async fn wait_invoice_streaming(
    stream: &mut Streaming<lnrpc::Invoice>,
    accepted: bool,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);

    while let Some(invoice) = tokio::time::timeout_at(deadline, stream.message()).await?? {
        let state = InvoiceState::try_from(invoice.state).unwrap_or_default();
        println!("  state={:?}", state);

        match state {
            InvoiceState::Settled => {
                println!("Settled: payment_hash={}", hex::encode(invoice.r_hash));
                return Ok(());
            }
            InvoiceState::Canceled => {
                println!("Cancelled");
                return Ok(());
            }
            InvoiceState::Accepted => {
                if accepted {
                    println!("Accepted");
                    return Ok(());
                }
            }
            _ => continue,
        }
    }

    anyhow::bail!("invoice stream closed before terminal state");
}

async fn create_hold_invoice(
    client: &mut Client,
    amount_sat: i64,
    memo: &str,
    payment_hash: Vec<u8>,
) -> Result<Response<invoicesrpc::AddHoldInvoiceResp>> {
    // AddHoldInvoice
    // https://lightning.engineering/api-docs/api/lnd/invoices/add-hold-invoice/
    let req = invoicesrpc::AddHoldInvoiceRequest {
        value: amount_sat,
        memo: memo.to_string(),
        hash: payment_hash,
        ..Default::default()
    };

    client
        .invoices()
        .add_hold_invoice(req)
        .await
        .context("failed to create hold invoice")
}

async fn settle_invoice(
    client: &mut Client,
    preimage: Vec<u8>,
) -> Result<Response<invoicesrpc::SettleInvoiceResp>> {
    // SettleInvoice
    // https://lightning.engineering/api-docs/api/lnd/invoices/settle-invoice/
    let req = invoicesrpc::SettleInvoiceMsg { preimage };

    client
        .invoices()
        .settle_invoice(req)
        .await
        .context("failed to settle hold invoice")
}

async fn track_payments(mut client: Client) -> Result<()> {
    // TrackPayments
    // https://lightning.engineering/api-docs/api/lnd/router/track-payments/
    let req = routerrpc::TrackPaymentsRequest {
        no_inflight_updates: false,
    };

    let mut stream = client
        .router()
        .track_payments(req)
        .await
        .context("failed to track payments")?;
    let stream = stream.get_mut();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    while let Some(payment) = tokio::time::timeout_at(deadline, stream.message()).await?? {
        println!("  tracking_payment={:?}", payment);
    }

    anyhow::bail!("payment stream closed before terminal state");
}

fn decode_file_path(path_str: &str) -> PathBuf {
    if let Some(s) = path_str.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            home.join(s)
        } else {
            std::path::PathBuf::from(path_str)
        }
    } else if let Some(s) = path_str.strip_prefix("$HOME/") {
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            home.join(s)
        } else {
            std::path::PathBuf::from(path_str)
        }
    } else if let Some(s) = path_str.strip_prefix("$PWD/") {
        if let Some(home) = std::env::var_os("PWD").map(std::path::PathBuf::from) {
            home.join(s)
        } else {
            std::path::PathBuf::from(path_str)
        }
    } else {
        std::path::PathBuf::from(path_str)
    }
}
