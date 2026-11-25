//! Creates regtest games using only the public admin roster and fixture operator keys.
#[path = "../../tests/payout_admin_common/mod.rs"]
mod payout_admin_common;

#[allow(dead_code)]
#[path = "../../tests/payout_admin_tools/mod.rs"]
mod payout_admin_tools;

use anyhow::{bail, ensure, Context, Result};
use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
    consensus::encode::deserialize_hex,
    secp256k1::Secp256k1,
    Address, Amount, Network, NetworkKind, OutPoint, Transaction,
};
use payout_admin_common::*;
use payout_admin_core::{
    block_payouts_tx::{self, ChangeOutput, ConnectorRef, FeeInput},
    deployment::{account_path, xonly_at_0_0},
    report::Report,
    test_support::Fixture,
    Deployment,
};
use payout_admin_tools::{electrum, Node};
use serde_json::json;
use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const DIRECTORY: &str = "/tmp/payout-test";

fn signer_identity(name: &str) -> Result<(Fingerprint, Xpub)> {
    let executable = env::current_exe()?.with_file_name(name);
    // The signer displays its private key on stderr only.
    let output = Command::new(executable).stderr(Stdio::null()).output()?;
    ensure!(output.status.success(), "{name} failed to load its key");
    let text = std::str::from_utf8(&output.stdout)?;
    let field = |prefix| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .context("missing signer identity field")
    };

    Ok((
        field("Fingerprint: ")?.parse()?,
        field("Account xpub: ")?.parse()?,
    ))
}

fn prompt<T>(label: &str, parse: impl Fn(&str) -> Option<T>) -> Result<T> {
    loop {
        eprint!("{label}: ");
        io::stderr().flush()?;
        let mut line = String::new();
        ensure!(
            io::stdin().read_line(&mut line)? != 0,
            "No Ledger identity supplied"
        );

        if let Some(value) = parse(line.trim()) {
            return Ok(value);
        }

        eprintln!("Invalid {label}. Try again.");
    }
}

async fn setup(directory: &Path, fixture: &Fixture) -> Result<()> {
    for name in ["admin2", "admin3"] {
        ensure!(
            Path::new("/tmp/payout-signers")
                .join(format!("{name}.tprv"))
                .is_file(),
            "Run ./admin2 and ./admin3 first."
        );
    }

    let second = signer_identity("admin2")?;
    let third = signer_identity("admin3")?;
    let account = prompt("Ledger account xpub (tpub)", |value| {
        value
            .parse::<Xpub>()
            .ok()
            .filter(|key| key.network == NetworkKind::Test && key.depth == 3)
    })?;
    let fingerprint = prompt("Ledger fingerprint (8 hex digits)", |value| {
        value.parse::<Fingerprint>().ok()
    })?;
    let mut roster = String::from("network = \"regtest\"\n");
    let mut keys = Vec::new();

    for (index, (fingerprint, account)) in [(fingerprint, account), second, third]
        .into_iter()
        .enumerate()
    {
        let number = index + 1;
        writeln!(roster, "\n[[admins]]\nlabel = \"Admin {number}\"\nfingerprint = \"{fingerprint}\"\nxpub = \"{account}\"")?;
        keys.push(format!("\"{}\"", xonly_at_0_0(&account)?));
    }

    println!("Creating the test claims. Wait until this prints the next step.");
    // The operator-only fixture supplies the protocol and operator schedule. A one-block NACK
    // timelock keeps setup short: each ACK otherwise waits for 144 blocks.
    let params = fixture
        .params_toml()
        .replacen(
            "pubkeys = []",
            &format!("pubkeys = [{}]", keys.join(", ")),
            1,
        )
        .replacen("nack_timelock = 144", "nack_timelock = 1", 1);
    let deployment = Deployment::load(&params, Some(&roster))?;
    call(&Node, "createwallet", json!(["payout-phys"])).await;
    fs::create_dir_all(directory)?;
    fs::write(directory.join("params.toml"), params)?;
    fs::write(directory.join("signers.toml"), roster)?;
    mine(&Node, 110).await;
    let address = Address::p2tr(
        &Secp256k1::new(),
        deployment.admin_keys()[0],
        None,
        deployment.network(),
    );
    // Several separate coins allow the report sets to be tested while earlier payouts remain pending.
    for _ in 0..6 {
        call(&Node, "sendtoaddress", json!([address.to_string(), 0.01])).await;
    }
    let n_of_n = n_of_n_keypair(fixture, &deployment);
    let mut mature_acks = Vec::new();

    for (number, count) in [(2, 3), (3, 20)] {
        let mut reports = Vec::new();

        for _ in 0..count {
            let report = game_report(fixture, &deployment, &n_of_n, true).await;
            mature_acks.push(report.counterproof_ack_txid());
            reports.push(report);
        }

        if number == 2 {
            reports.push(game_report(fixture, &deployment, &n_of_n, false).await);
        }

        fs::write(
            directory.join(format!("report{number}.json")),
            serde_json::to_vec(&reports)?,
        )?;
    }

    mine(&Node, 4).await;
    // Always last: no more blocks after the young ACK reaches two confirmations.
    let young = game_report(fixture, &deployment, &n_of_n, true).await;
    fs::write(directory.join("report1.json"), serde_json::to_vec(&young)?)?;
    let mut indexed = false;
    for _ in 0..120 {
        if electrum(
            "blockchain.transaction.get",
            json!([young.counterproof_ack_txid().to_string(), true]),
        )
        .is_ok_and(|response| response["confirmations"] == 2)
        {
            indexed = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    ensure!(indexed, "electrs did not index the young ACK in time");

    for txid in mature_acks {
        let ack = electrum(
            "blockchain.transaction.get",
            json!([txid.to_string(), true]),
        )
        .map_err(|error| anyhow::anyhow!("electrs did not return the ACK {txid}: {error}"))?;
        ensure!(
            ack["confirmations"]
                .as_u64()
                .is_some_and(|count| 6 <= count),
            "electrs reports fewer than 6 confirmations for the ACK {txid}"
        );
    }

    println!("Start the coordinator with ./server.");

    Ok(())
}

async fn game_report(
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &bitcoin::secp256k1::Keypair,
    broadcast_ack: bool,
) -> Report {
    let (old, ack) = if broadcast_ack {
        acked_game(&Node, fixture, deployment, n_of_n).await
    } else {
        build_game_with_ack(&WalletFunding(&Node), fixture, deployment, n_of_n).await
    };
    let new = claim_and_contest(&Node, fixture, deployment, n_of_n).await;
    mine(&Node, 1).await;

    Report::from_value(json!({
        "network": "regtest", "operatorPubkey": fixture.operator_xonly(0),
        "newClaimTxid": new.claim.compute_txid(), "newContestTxid": new.contest.compute_txid(),
        "ackedClaimTxid": old.claim.compute_txid(), "ackedContestTxid": old.contest.compute_txid(),
        "counterproofAckTxid": ack.compute_txid(),
    }), bitcoin::Network::Regtest).unwrap()
}

async fn fresh_psbt(directory: &Path, fixture: &Fixture) -> Result<bitcoin::Psbt> {
    let deployment = Deployment::load(
        &fs::read_to_string(directory.join("params.toml"))?,
        Some(&fs::read_to_string(directory.join("signers.toml"))?),
    )?;
    let signer = &deployment.signers()?[1];
    let secp = Secp256k1::new();
    let n_of_n = n_of_n_keypair(fixture, &deployment);
    let game = claim_and_contest(&Node, fixture, &deployment, &n_of_n).await;

    // Reserve each receiving index once, including concurrent or interrupted invocations.
    let mut index = 1u32;
    loop {
        match fs::File::create_new(directory.join(format!("fee-{index}"))) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => index += 1,
            Err(error) => return Err(error.into()),
        }

        ChildNumber::from_normal_idx(index)?;
    }

    let receive = DerivationPath::from(vec![
        ChildNumber::Normal { index: 0 },
        ChildNumber::from_normal_idx(index)?,
    ]);
    let internal_key = signer
        .account_xpub()
        .derive_pub(&secp, &receive)?
        .public_key
        .x_only_public_key()
        .0;
    let address = Address::p2tr(&secp, internal_key, None, Network::Regtest);
    let id = call(&Node, "sendtoaddress", json!([address.to_string(), 0.001])).await;
    let raw = call(&Node, "getrawtransaction", json!([id])).await;
    let transaction: Transaction =
        deserialize_hex(raw.as_str().context("missing funding transaction")?)?;
    let (position, output) = transaction
        .output
        .iter()
        .enumerate()
        .find(|(_, output)| output.script_pubkey == address.script_pubkey())
        .context("missing fee output")?;
    let account = account_path(Network::Regtest);
    let fee = FeeInput::new(
        OutPoint::new(transaction.compute_txid(), position.try_into()?),
        output,
        internal_key,
        signer.fingerprint(),
        account.extend(&receive),
    )?;
    let change_path: DerivationPath = "m/1/0".parse()?;
    let change_key = signer
        .account_xpub()
        .derive_pub(&secp, &change_path)?
        .public_key
        .x_only_public_key()
        .0;
    let change = ChangeOutput::new(
        Amount::from_sat(97_000),
        change_key,
        signer.fingerprint(),
        account.extend(&change_path),
    )?;
    mine(&Node, 6).await;

    for _ in 0..120 {
        if electrum(
            "blockchain.transaction.get",
            json!([game.claim.compute_txid(), true]),
        )
        .is_ok_and(|response| {
            response["confirmations"]
                .as_u64()
                .is_some_and(|count| 6 <= count)
        }) {
            return Ok(block_payouts_tx::build_psbt(
                &deployment,
                &[ConnectorRef::new(game.claim.compute_txid())],
                &[fee],
                &change,
            )?);
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    bail!("electrs did not index the new claim")
}

#[tokio::main]
async fn main() -> Result<()> {
    env::set_var("PAYOUT_TEST_RPC", "http://127.0.0.1:18500");
    env::set_var("PAYOUT_TEST_ELECTRUM", "127.0.0.1:18501");
    let directory = Path::new(DIRECTORY);
    let arguments: Vec<_> = env::args().skip(1).collect();

    match arguments.as_slice() {
        [command] if command == "setup" => setup(directory, &Fixture::new(0, 2)).await?,
        [command] if command == "psbt" => {
            println!("{}", fresh_psbt(directory, &Fixture::new(0, 2)).await?)
        }
        [command] if command == "mine" => mine(&Node, 4).await,
        [command, number] if command == "report" && matches!(number.as_str(), "1" | "2" | "3") => {
            println!(
                "{}",
                fs::read_to_string(directory.join(format!("report{number}.json")))?
            )
        }
        _ => bail!("Usage: regtest-bridge setup | report <1|2|3> | psbt | mine"),
    }

    Ok(())
}
