//! Pure construction and signature verification for the single-key Admin Wallet.
use crate::{
    fees,
    payout_tx::{ChangeOutput, FeeInput},
};
use bitcoin::{
    absolute, psbt::Psbt, transaction, Address, Amount, Network, ScriptBuf, Sequence, Transaction,
    TxIn, TxOut, Witness,
};

pub fn destination(value: &str, network: Network) -> Result<Address, String> {
    let address = value
        .parse::<Address<bitcoin::address::NetworkUnchecked>>()
        .map_err(|_| "Destination must be a bitcoin address.".to_string())?;
    let address = address.require_network(network).map_err(|_| {
        format!(
            "Destination must be a {} bitcoin address.",
            if network == Network::Bitcoin {
                "mainnet"
            } else {
                "testnet"
            }
        )
    })?;
    let s = address.script_pubkey();
    if !(s.is_p2pkh() || s.is_p2sh() || s.is_p2wpkh() || s.is_p2wsh() || s.is_p2tr()) {
        return Err("Destination must be a bitcoin address.".into());
    }
    Ok(address)
}

/// The search bound must match the node's descriptor scan range.
pub fn first_unused_change(
    last_index: u32,
    mut script_at: impl FnMut(u32) -> Result<ScriptBuf, String>,
    is_used: impl Fn(u32) -> bool,
    taken: &std::collections::HashSet<ScriptBuf>,
) -> Result<Option<u32>, String> {
    for i in 0..=last_index {
        if !is_used(i) && !taken.contains(&script_at(i)?) {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub struct BuiltSend {
    pub psbt: Psbt,
    pub fee_sat: u64,
    pub vsize: u64,
}

fn transaction(coins: &[FeeInput], outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: coins
            .iter()
            .map(|c| TxIn {
                previous_output: c.outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: outputs,
    }
}
fn size(tx: &Transaction) -> u64 {
    let mut signed = tx.clone();
    for input in &mut signed.input {
        input.witness.push([0; 64]);
    }
    signed.vsize() as u64
}

/// Required coins stay in order for replacement; additional confirmed coins are largest first.
pub fn build(
    required: &[FeeInput],
    available: &[FeeInput],
    recipient: ScriptBuf,
    amount: Option<u64>,
    rate: u64,
    change: &ChangeOutput,
) -> Result<BuiltSend, String> {
    fees::check_fee_rate(rate).map_err(|e| e.to_string())?;
    let mut candidates = available.to_vec();
    candidates.retain(|c| !required.iter().any(|r| r.outpoint == c.outpoint));
    candidates.sort_by_key(|c| (std::cmp::Reverse(c.txout.value.to_sat()), c.outpoint));
    let mut selected = required.to_vec();
    if amount.is_none() {
        selected.append(&mut candidates);
    }
    let mut candidates = candidates.into_iter();
    loop {
        let total: u64 = selected.iter().map(|c| c.txout.value.to_sat()).sum();
        let output = TxOut {
            value: Amount::ZERO,
            script_pubkey: recipient.clone(),
        };
        let mut tx = transaction(&selected, vec![output]);
        let no_change_fee = fees::fee_for(rate, size(&tx));
        let wanted = amount.unwrap_or(total.saturating_sub(no_change_fee));
        if !selected.is_empty() && total >= wanted.saturating_add(no_change_fee) {
            if wanted < recipient.minimal_non_dust().to_sat() {
                return Err("Recipient amount is below the dust limit.".into());
            }
            tx.output[0].value = Amount::from_sat(wanted);
            if amount.is_some() {
                tx.output.push(TxOut {
                    value: Amount::ZERO,
                    script_pubkey: change.script_pubkey(),
                });
                let fee = fees::fee_for(rate, size(&tx));
                let remainder = total.saturating_sub(wanted.saturating_add(fee));
                if remainder >= 330 {
                    tx.output[1].value = Amount::from_sat(remainder);
                } else {
                    tx.output.pop();
                }
            }
            let vsize = size(&tx);
            let fee_sat = total - tx.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
            let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|e| e.to_string())?;
            for (input, coin) in psbt.inputs.iter_mut().zip(&selected) {
                if !coin.txout.script_pubkey.is_p2tr() {
                    return Err("Wallet input is not taproot.".into());
                }
                coin.fill_input(input);
            }
            if psbt.outputs.len() == 2 {
                change.fill_output(&mut psbt.outputs[1]);
            }
            return Ok(BuiltSend {
                psbt,
                fee_sat,
                vsize,
            });
        }
        match candidates.next() {
            Some(c) => selected.push(c),
            None => return Err("Insufficient funds".into()),
        }
    }
}

pub fn check_replacement(
    old_fee: u64,
    old_size: u64,
    new: &BuiltSend,
    incremental_tenths: u64,
) -> Result<(), String> {
    if new.fee_sat as u128 * old_size as u128 <= old_fee as u128 * new.vsize as u128
        || new.fee_sat < old_fee.saturating_add(fees::fee_for(incremental_tenths, new.vsize))
    {
        return Err(format!("The new fee must be at least {} sat and the new fee rate must be higher than {:.1} sat/vB.", old_fee.saturating_add(fees::fee_for(incremental_tenths, new.vsize)), old_fee as f64 / old_size as f64));
    }
    Ok(())
}

/// A replacement also evicts children, so its extra relay fee is paid above their total fees.
pub fn check_descendant_fee(
    descendant_fee: u64,
    new: &BuiltSend,
    incremental_tenths: u64,
) -> Result<(), String> {
    let minimum = descendant_fee.saturating_add(fees::fee_for(incremental_tenths, new.vsize));
    if new.fee_sat < minimum {
        let tenths = minimum.saturating_mul(10).div_ceil(new.vsize);
        return Err(format!(
            "The new fee rate must be at least {}.{} sat/vB to pay for the evicted descendants.",
            tenths / 10,
            tenths % 10
        ));
    }
    Ok(())
}

/// Verify against the original prevouts and unsigned transaction, never device-supplied metadata.
pub fn finalize(original: &Psbt, signed: &Psbt) -> Result<Transaction, String> {
    use bitcoin::{
        hashes::Hash,
        secp256k1::{Message, Secp256k1, XOnlyPublicKey},
        sighash::{Prevouts, SighashCache},
        TapSighashType,
    };
    if original.unsigned_tx != signed.unsigned_tx || original.inputs.len() != signed.inputs.len() {
        return Err("The device changed the transaction.".into());
    }
    let prevouts: Vec<_> = original
        .inputs
        .iter()
        .map(|i| i.witness_utxo.clone().ok_or("Missing prevout"))
        .collect::<Result<_, _>>()?;
    let mut cache = SighashCache::new(&original.unsigned_tx);
    let mut tx = original.unsigned_tx.clone();
    for (i, input) in signed.inputs.iter().enumerate() {
        let sig = input
            .tap_key_sig
            .ok_or("The device did not sign every wallet input.")?;
        if sig.sighash_type != TapSighashType::Default {
            return Err("Unexpected signature hash type.".into());
        }
        let script = prevouts[i].script_pubkey.as_bytes();
        if !prevouts[i].script_pubkey.is_p2tr() {
            return Err("Invalid wallet prevout.".into());
        }
        let key = XOnlyPublicKey::from_slice(&script[2..]).map_err(|e| e.to_string())?;
        let hash = cache
            .taproot_key_spend_signature_hash(i, &Prevouts::All(&prevouts), TapSighashType::Default)
            .map_err(|e| e.to_string())?;
        Secp256k1::verification_only()
            .verify_schnorr(
                &sig.signature,
                &Message::from_digest(hash.to_byte_array()),
                &key,
            )
            .map_err(|_| "Invalid wallet signature.".to_string())?;
        tx.input[i].witness.push(sig.to_vec());
    }
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{change, fee_coin, Fixture};
    #[test]
    fn change_skips_used_and_reserved_scripts() {
        let script = |i| Ok(ScriptBuf::from_bytes(vec![i as u8]));
        let taken = std::collections::HashSet::from([script(1).unwrap()]);
        assert_eq!(
            first_unused_change(3, script, |i| i == 0, &taken).unwrap(),
            Some(2)
        );
        assert_eq!(
            first_unused_change(1, script, |i| i == 0, &taken).unwrap(),
            None
        );
    }

    #[test]
    fn selection_max_dust_and_insufficient_funds() {
        let fx = Fixture::new(3, 2);
        let coins = [fee_coin(&fx, 0, 0, 10_000), fee_coin(&fx, 0, 1, 50_000)];
        let change = change(&fx, 0);
        let recipient = change.script_pubkey();
        let built = build(&[], &coins, recipient.clone(), Some(20_000), 10, &change).unwrap();
        assert_eq!(built.psbt.unsigned_tx.input.len(), 1);
        assert_eq!(
            built.psbt.unsigned_tx.input[0].previous_output,
            coins[1].outpoint
        );
        assert_eq!(built.vsize, 154);
        let max = build(&[], &coins, recipient.clone(), None, 10, &change).unwrap();
        assert_eq!(max.psbt.unsigned_tx.output.len(), 1);
        assert_eq!(
            max.psbt.unsigned_tx.output[0].value.to_sat() + max.fee_sat,
            60_000
        );
        assert_eq!(
            build(&[], &coins, recipient.clone(), Some(60_000), 10, &change)
                .err()
                .unwrap(),
            "Insufficient funds"
        );
        assert!(build(&[], &coins, recipient.clone(), Some(329), 10, &change).is_err());
        let dust = build(&[], &coins[1..], recipient, Some(49_600), 10, &change).unwrap();
        assert_eq!(dust.psbt.unsigned_tx.output.len(), 1);
        assert_eq!(dust.fee_sat, 400);
    }
    #[test]
    fn verifies_every_signature_against_original_transaction() {
        let fx = Fixture::new(3, 2);
        let coins = [fee_coin(&fx, 0, 0, 10_000), fee_coin(&fx, 0, 1, 10_000)];
        let change = change(&fx, 0);
        let built = build(&[], &coins, change.script_pubkey(), None, 20, &change).unwrap();
        let mut signed = built.psbt.clone();
        crate::test_support::sign_fee(&fx, &mut signed, &coins[0], 0);
        assert!(finalize(&built.psbt, &signed).is_err());
        crate::test_support::sign_fee(&fx, &mut signed, &coins[1], 1);
        assert_eq!(
            finalize(&built.psbt, &signed).unwrap().vsize() as u64,
            built.vsize
        );
        signed.inputs.swap(0, 1);
        assert!(finalize(&built.psbt, &signed).is_err());
    }

    #[test]
    fn network_and_invalid_destination_texts() {
        let fx = Fixture::new(3, 2);
        let script = change(&fx, 0).script_pubkey();
        let main = Address::from_script(&script, Network::Bitcoin).unwrap();
        let test = Address::from_script(&script, Network::Regtest).unwrap();
        assert_eq!(
            destination("garbage", Network::Bitcoin).unwrap_err(),
            "Destination must be a bitcoin address."
        );
        assert_eq!(
            destination(&main.to_string(), Network::Regtest).unwrap_err(),
            "Destination must be a testnet bitcoin address."
        );
        assert_eq!(
            destination(&test.to_string(), Network::Bitcoin).unwrap_err(),
            "Destination must be a mainnet bitcoin address."
        );
    }
    #[test]
    fn replacement_pays_descendants_and_incremental_relay_fee() {
        let fx = Fixture::new(3, 2);
        let coins = [fee_coin(&fx, 0, 0, 100_000)];
        let change = change(&fx, 0);
        let new = build(
            &[],
            &coins,
            change.script_pubkey(),
            Some(20_000),
            50,
            &change,
        )
        .unwrap();
        assert!(check_descendant_fee(10_308, &new, 10)
            .unwrap_err()
            .contains("68.0 sat/vB"));
        check_descendant_fee(new.fee_sat - new.vsize, &new, 10).unwrap();
        assert!(check_descendant_fee(new.fee_sat - new.vsize, &new, 20).is_err());
    }

    #[test]
    fn replacement_minimum_and_max_amount() {
        let fx = Fixture::new(3, 2);
        let coins = [fee_coin(&fx, 0, 0, 10_000)];
        let change = change(&fx, 0);
        let old = build(&[], &coins, change.script_pubkey(), None, 10, &change).unwrap();
        let low = build(&coins, &[], change.script_pubkey(), None, 11, &change).unwrap();
        assert!(check_replacement(old.fee_sat, old.vsize, &low, 10).is_err());
        let new = build(&coins, &[], change.script_pubkey(), None, 30, &change).unwrap();
        check_replacement(old.fee_sat, old.vsize, &new, 10).unwrap();
        assert!(new.psbt.unsigned_tx.output[0].value < old.psbt.unsigned_tx.output[0].value);
        assert!(finalize(&new.psbt, &new.psbt).is_err());
    }
}
