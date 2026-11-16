//! Ledger (BIP-388) wallet policies for the Payout Administrators.
//!
//! Admin multisig, one per covenant (N/N key):
//!
//! ```text
//! tr(@0/**,multi_a(k,@1/**,…,@n/**))
//! @0 = BIP-328 synthetic xpub of the N/N key (no key origin)
//! @i = admin i's account xpub with origin [fpr/86'/coin'/73'], in params.toml order
//! ```
//!
//! At receive index 0, `@0/0/0` is the connector's internal key and `@i/0/0` is admin i's key in
//! `params.toml`, so the policy's first receive address is exactly the claim's admin payout
//! connector of that covenant (checked in the tests below). The Ledger therefore recognises the
//! connector inputs as its own and signs their leaf.
//!
//! The Admin Wallet (fee inputs and change) is the standard policy `tr(@0/**)` on the same account.

use bitcoin::{Address, Network};
use serde::Serialize;

use crate::deployment::{AdminSigner, Covenant, Deployment, DeploymentError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySpec {
    /// At most 16 ASCII bytes (Ledger limit). Empty for the default policy.
    pub name: String,
    pub template: String,
    pub keys: Vec<String>,
}

/// Ledger policy name for a covenant: `Payout <first 3 bytes of the N/N key>` (13 bytes).
pub fn admin_policy_name(cov: &Covenant) -> String {
    format!("Payout {}", cov.short_id())
}

pub fn admin_multisig_template(n_admins: usize, threshold: usize) -> String {
    let slots = (1..=n_admins)
        .map(|i| format!("@{i}/**"))
        .collect::<Vec<_>>()
        .join(",");
    format!("tr(@0/**,multi_a({threshold},{slots}))")
}

pub fn admin_multisig_policy(
    dep: &Deployment,
    cov: &Covenant,
) -> Result<PolicySpec, DeploymentError> {
    let signers = dep.signers()?;
    let mut keys = vec![cov.synthetic_xpub(dep.network).to_string()];
    keys.extend(signers.iter().map(|s| s.policy_key(dep.network)));
    Ok(PolicySpec {
        name: admin_policy_name(cov),
        template: admin_multisig_template(signers.len(), dep.threshold),
        keys,
    })
}

pub fn admin_wallet_policy(signer: &AdminSigner, network: Network) -> PolicySpec {
    PolicySpec {
        name: String::new(),
        template: "tr(@0/**)".into(),
        keys: vec![signer.policy_key(network)],
    }
}

/// The policy as an output descriptor (keys expanded at `branch`), e.g. for `deriveaddresses`.
pub fn policy_descriptor(p: &PolicySpec, branch: u32) -> String {
    let mut out = p.template.clone();
    // Replace higher indices first so "@1" does not match inside "@10".
    for (i, key) in p.keys.iter().enumerate().rev() {
        out = out.replace(&format!("@{i}/**"), &format!("{key}/{branch}/*"));
    }
    out
}

/// The connector address of a covenant, for display.
pub fn connector_address(dep: &Deployment, cov: &Covenant) -> Address {
    Address::from_script(&cov.connector_script, dep.network).expect("p2tr script")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use bitcoin::bip32::{ChildNumber, Xpub};
    use bitcoin::secp256k1::Secp256k1;
    use std::str::FromStr;

    /// Evaluates `tr(@0/**,multi_a(k,@1/**,…))` at receive index 0 by hand and checks that it is
    /// the bridge connector: the Ledger signs exactly what the bridge locks.
    #[test]
    fn first_receive_address_is_the_connector() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let cov = &dep.covenants[0];
        let p = admin_multisig_policy(&dep, cov).unwrap();
        assert_eq!(p.template, "tr(@0/**,multi_a(2,@1/**,@2/**,@3/**))");
        assert!(p.name.len() <= 16);
        let secp = Secp256k1::verification_only();
        let at00 = |key: &str| {
            let xpub_str = key.rsplit(']').next().unwrap();
            let x = Xpub::from_str(xpub_str).unwrap();
            x.derive_pub(
                &secp,
                &[
                    ChildNumber::Normal { index: 0 },
                    ChildNumber::Normal { index: 0 },
                ],
            )
            .unwrap()
            .public_key
            .x_only_public_key()
            .0
        };
        let internal = at00(&p.keys[0]);
        let admins: Vec<_> = p.keys[1..].iter().map(|k| at00(k)).collect();
        assert_eq!(admins, dep.admin_keys);
        let leaf = strata_bridge_primitives::scripts::prelude::threshold_multisig_script(
            &admins,
            dep.threshold,
        );
        let spend = bitcoin::taproot::TaprootBuilder::new()
            .add_leaf(0, leaf)
            .unwrap()
            .finalize(&Secp256k1::new(), internal)
            .unwrap();
        let script = bitcoin::ScriptBuf::new_p2tr_tweaked(spend.output_key());
        assert_eq!(script, cov.connector_script);
    }

    #[test]
    fn descriptor_expands_keys() {
        let fx = Fixture::new(2, 1);
        let dep = fx.deployment();
        let p = admin_wallet_policy(&dep.signers().unwrap()[0], dep.network);
        let d = policy_descriptor(&p, 1);
        assert!(d.starts_with("tr(["), "{d}");
        assert!(d.ends_with("/1/*)"), "{d}");
    }
}
