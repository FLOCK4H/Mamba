use {
    anyhow::{Context, Result},
    base64::{Engine as _, engine::general_purpose::STANDARD as B64},
    solana_pubkey::Pubkey,
    solana_transaction::versioned::VersionedTransaction,
    std::{env, io::Read, str::FromStr},
};

fn main() -> Result<()> {
    let mut expected_signer: Option<Pubkey> = None;
    let mut base64_tx: Option<String> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--expected-signer" => {
                let raw = args
                    .next()
                    .context("--expected-signer requires a pubkey argument")?;
                expected_signer =
                    Some(Pubkey::from_str(raw.trim()).context("invalid --expected-signer pubkey")?);
            }
            "--help" | "-h" => {
                eprintln!(
                    "mamba_tx_inspect\n\nUSAGE:\n  mamba_tx_inspect [--expected-signer <PUBKEY>] [<TX_BASE64>]\n\nIf <TX_BASE64> is omitted, reads from stdin.\n"
                );
                return Ok(());
            }
            _ => {
                if base64_tx.is_some() {
                    anyhow::bail!("unexpected extra argument: {arg}");
                }
                base64_tx = Some(arg);
            }
        }
    }

    let base64_tx = match base64_tx {
        Some(value) => value,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("read tx base64 from stdin")?;
            buf
        }
    };

    let wire = B64
        .decode(base64_tx.trim())
        .context("base64 decode transaction")?;
    let tx: VersionedTransaction = bincode::deserialize(&wire).context("bincode decode tx")?;

    let required_signatures = tx.message.header().num_required_signatures as usize;
    let signer_slice = tx
        .message
        .static_account_keys()
        .get(0..required_signatures)
        .context("transaction has fewer static account keys than required signatures")?;
    let signers: Vec<Pubkey> = signer_slice
        .iter()
        .map(|addr| Pubkey::new_from_array(*addr.as_array()))
        .collect();

    let signatures_len = tx.signatures.len();
    let signatures_all_zero = tx
        .signatures
        .iter()
        .all(|sig| *sig == solana_signature::Signature::default());

    let expected_ok = expected_signer.map(|expected| {
        required_signatures == 1 && signers.first().is_some_and(|signer| *signer == expected)
    });

    let out = serde_json::json!({
        "required_signatures": required_signatures,
        "signatures_len": signatures_len,
        "signatures_all_zero": signatures_all_zero,
        "signers": signers.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "expected_signer_ok": expected_ok,
    });

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
