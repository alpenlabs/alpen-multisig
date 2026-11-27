//! Software signer for the regtest tester kit. The invocation name selects the wallet.
use anyhow::{bail, ensure, Context, Result};
use bitcoin::{
    bip32::{Xpriv, Xpub},
    hashes::Hash,
    key::TapTweak,
    psbt::Psbt,
    secp256k1::{schnorr, Message, Secp256k1},
    sighash::{Prevouts, SighashCache},
    taproot::Signature,
    Network,
};
use payout_admin_core::deployment::account_path;
use rand::{rngs::OsRng, RngCore};
use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::Path,
};

fn master_key(path: &Path, create: bool) -> Result<Xpriv> {
    if create && !path.try_exists()? {
        let directory = path.parent().context("missing key directory")?;
        fs::create_dir_all(directory)?;
        let mut seed = [0; 32];
        OsRng.try_fill_bytes(&mut seed)?;
        let master = Xpriv::new_master(Network::Regtest, &seed)?;
        fs::write(path, format!("{master}\n"))?;
    }

    Ok(fs::read_to_string(path)
        .with_context(|| {
            format!(
                "Reading {} (run the signer without arguments first)",
                path.display()
            )
        })?
        .trim()
        .parse()?)
}

fn sign(master: &Xpriv, command: &str, psbt: &mut Psbt) -> Result<String> {
    let secp = Secp256k1::new();
    let fingerprint = master.fingerprint(&secp);
    let transaction_id = psbt.unsigned_tx.compute_txid();
    let mut cache = SighashCache::new(&psbt.unsigned_tx);
    let outputs = psbt
        .inputs
        .iter()
        .map(|input| input.witness_utxo.clone().context("missing witness UTXO"))
        .collect::<Result<Vec<_>>>()?;
    let prevouts = Prevouts::All(&outputs);
    let mut signed = false;
    let mut lines = String::new();

    for (index, input) in psbt.inputs.iter_mut().enumerate() {
        let sighash_type = input.taproot_hash_ty()?;

        for (public_key, (leaves, (origin, path))) in &input.tap_key_origins {
            if origin != &fingerprint {
                continue;
            }

            let keypair = master.derive_priv(&secp, path)?.to_keypair(&secp);

            if keypair.x_only_public_key().0 != *public_key {
                continue;
            }

            let signature = |digest, signing_key: &_| -> Result<Signature> {
                let mut signature = secp.sign_schnorr(&Message::from_digest(digest), signing_key);

                if command == "add-bad-sig" {
                    let mut bytes = signature.serialize();
                    bytes[0] ^= 1;
                    signature = schnorr::Signature::from_slice(&bytes)?;
                }

                Ok(Signature {
                    signature,
                    sighash_type,
                })
            };

            if command != "sign" && leaves.is_empty() && input.tap_internal_key == Some(*public_key)
            {
                let hash =
                    cache.taproot_key_spend_signature_hash(index, &prevouts, sighash_type)?;
                input.tap_key_sig = Some(signature(
                    hash.to_byte_array(),
                    &keypair.tap_tweak(&secp, input.tap_merkle_root).to_keypair(),
                )?);
                signed = true;
            }

            for leaf in leaves {
                let hash = cache.taproot_script_spend_signature_hash(
                    index,
                    &prevouts,
                    *leaf,
                    sighash_type,
                )?;
                let signature = signature(hash.to_byte_array(), &keypair)?;
                input
                    .tap_script_sigs
                    .insert((*public_key, *leaf), signature);
                signed = true;

                if command == "sign" {
                    writeln!(
                        lines,
                        "{transaction_id}:{index}:{public_key}:{}",
                        signature.signature
                    )?;
                }
            }
        }
    }

    ensure!(signed, "No matching inputs to sign.");

    if command == "sign" {
        return Ok(lines);
    }

    Ok(psbt.to_string())
}

fn main() -> Result<()> {
    let arguments: Vec<_> = env::args().collect();
    let name = Path::new(&arguments[0])
        .file_name()
        .and_then(|name| name.to_str())
        .context("missing signer name")?;
    let path = Path::new("/tmp/payout-signers").join(format!("{name}.tprv"));

    match &arguments[1..] {
        [] => {
            let master = master_key(&path, true)?;
            let secp = Secp256k1::new();
            let account = master.derive_priv(&secp, &account_path(Network::Regtest))?;
            println!("Name: {name}");
            println!("Fingerprint: {}", master.fingerprint(&secp));
            println!("Account xpub: {}", Xpub::from_priv(&secp, &account));
            eprintln!("Master tprv: {master}");
        }
        [command] if matches!(command.as_str(), "sign" | "add-sig" | "add-bad-sig") => {
            let master = master_key(&path, false)?;
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            println!(
                "{}",
                sign(&master, command, &mut text.trim().parse()?)?.trim_end()
            );
        }
        _ => bail!("Usage: {name} [sign|add-sig|add-bad-sig]"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use payout_admin_core::{
        block_payouts_tx::{self, ConnectorRef},
        test_support::{change, fee_coin, Fixture},
    };

    #[test]
    fn signing_commands_match_paste_and_import() -> Result<()> {
        let master = Xpriv::new_master(Network::Regtest, &[42; 32])?;
        let mut fixture = Fixture::new(3, 2);
        fixture.replace_admin_master(1, master);
        let deployment = fixture.deployment();
        let claims =
            [1, 2].map(|index| ConnectorRef::new(fixture.game(&deployment, index).claim_txid()));
        let base = block_payouts_tx::build_psbt(
            &deployment,
            &claims,
            &[fee_coin(&fixture, 1, 7, 50_000)],
            &change(&fixture, 49_000),
        )?;
        let lines = sign(&master, "sign", &mut base.clone())?;
        let mut pasted = base.clone();
        assert_eq!(
            block_payouts_tx::paste_signatures(&deployment, &mut pasted, &lines)
                .added()
                .len(),
            2
        );

        for (command, valid) in [("add-sig", true), ("add-bad-sig", false)] {
            let encoded = sign(&master, command, &mut base.clone())?;
            let imported = block_payouts_tx::import_psbt(&deployment, &encoded)?;
            let analysis = block_payouts_tx::analyze(&deployment, &imported)?;
            assert_eq!(analysis.signed_by(1), valid);
            assert_eq!(analysis.fee_inputs()[0].signed(), valid);
        }

        let unrelated = Xpriv::new_master(Network::Regtest, &[43; 32])?;
        assert!(sign(&unrelated, "sign", &mut base.clone()).is_err());

        Ok(())
    }
}
