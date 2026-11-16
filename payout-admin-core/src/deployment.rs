//! The trusted deployment: strata-bridge `params.toml` plus the Payout Administrator signer roster.
//!
//! `params.toml` is the bridge's consensus parameter file. From it come:
//!
//! - the admin multisig: `[keys.admin] pubkeys` (x-only, in order) and `threshold`;
//! - the operator schedule `[[keys.operators]]`, from which every N/N key the bridge has used is
//!   derived (MuSig2 KeyAgg of the even-lifted covenant keys of the operators active together,
//!   in operator-index order — the bridge's `CovenantId::from_operator_table`).
//!
//! Each N/N key defines one *covenant*, and each covenant one admin payout connector: the bridge's
//! [`ClaimAdminPayoutConnector`] over that N/N key and the admin set.
//!
//! The bridge file holds only x-only admin keys. A Ledger needs every cosigner's account xpub to
//! register the admin multisig policy, so a separate [`SignerRoster`] lists them, and
//! [`Deployment::with_roster`] checks that each `xpub/0/0` is exactly the admin key at the same
//! position in `params.toml`. The roster can therefore add no key of its own.

use std::{collections::BTreeSet, str::FromStr};

use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
    hashes::{sha256, Hash},
    secp256k1::{PublicKey, XOnlyPublicKey},
    Network, NetworkKind, ScriptBuf, TxOut,
};
use musig2::KeyAggContext;
use serde::{Deserialize, Serialize};
use strata_bridge_common::params::Params;
use strata_bridge_connectors::{prelude::ClaimAdminPayoutConnector, Connector};

/// BIP-328 chain code of MuSig2 synthetic xpubs (the bridge's `key-deriv` crate uses the same).
pub const BIP328_CHAIN_CODE: [u8; 32] = [
    0x86, 0x80, 0x87, 0xca, 0x02, 0xa6, 0xf9, 0x74, 0xc4, 0x59, 0x89, 0x24, 0xc3, 0x6b, 0x57, 0x76,
    0x2d, 0x32, 0xcb, 0x45, 0x71, 0x71, 0x67, 0xe3, 0x00, 0x62, 0x2c, 0x71, 0x67, 0xe3, 0x89, 0x65,
];

/// Ledger Bitcoin app limit on `multi_a` keys.
pub const LEDGER_MAX_MULTI_A_KEYS: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeploymentError {
    #[error("params.toml: {0}")]
    Params(String),
    #[error("signer roster: {0}")]
    Roster(String),
}

/// One N/N key of the bridge and the admin payout connector it defines.
#[derive(Debug, Clone)]
pub struct Covenant {
    /// Position in [`Deployment::covenants`].
    pub index: usize,
    /// Full N/N aggregate key (needed for the BIP-328 internal key of the connector).
    pub n_of_n: PublicKey,
    /// The x-only N/N key as it appears in the claim-contest connector's leaves.
    pub n_of_n_xonly: XOnlyPublicKey,
    /// Operators whose covenant keys form this N/N key, by operator index.
    pub operator_indices: Vec<u32>,
    /// First height at which this exact operator set is active.
    pub from_height: u64,
    pub connector: ClaimAdminPayoutConnector,
    pub connector_script: ScriptBuf,
}

impl Covenant {
    pub fn connector_tx_out(&self) -> TxOut {
        self.connector.tx_out()
    }

    /// Short identifier shown to signers and used in Ledger policy names.
    pub fn short_id(&self) -> String {
        hex::encode(&self.n_of_n.serialize()[1..4])
    }

    /// The BIP-328 synthetic xpub of the N/N key (depth 0, BIP-328 chain code).
    pub fn synthetic_xpub(&self, network: Network) -> Xpub {
        synthetic_xpub(self.n_of_n, network)
    }
}

/// BIP-328: an xpub with the aggregate key at depth 0 and the fixed chain code.
pub fn synthetic_xpub(aggregate: PublicKey, network: Network) -> Xpub {
    Xpub {
        network: NetworkKind::from(network),
        depth: 0,
        parent_fingerprint: Fingerprint::default(),
        child_number: ChildNumber::Normal { index: 0 },
        public_key: aggregate,
        chain_code: BIP328_CHAIN_CODE.into(),
    }
}

/// An operator from the schedule (only what the Payout Administrator needs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorInfo {
    pub index: u32,
    pub covenant_key: String,
    pub activation_height: u64,
    pub deactivation_height: Option<u64>,
}

/// One Payout Administrator as the roster lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSigner {
    /// Position in `[keys.admin].pubkeys`.
    pub index: usize,
    pub label: String,
    /// The admin key as in `params.toml` (`account_xpub/0/0`).
    #[serde(serialize_with = "ser_xonly")]
    pub xonly: XOnlyPublicKey,
    #[serde(serialize_with = "ser_display")]
    pub fingerprint: Fingerprint,
    /// Account xpub at [`account_path`].
    #[serde(serialize_with = "ser_display")]
    pub account_xpub: Xpub,
}

impl AdminSigner {
    /// The key origin of the admin key: `[fingerprint/86'/coin'/73'/0/0]`.
    pub fn key_path(&self, network: Network) -> DerivationPath {
        admin_key_path(network)
    }

    /// `[fpr/86'/coin'/73']xpub` as a Ledger policy key / descriptor key.
    pub fn policy_key(&self, network: Network) -> String {
        format!(
            "[{}/{}]{}",
            self.fingerprint,
            account_path(network)
                .to_string()
                .trim_start_matches("m/")
                .replace('h', "'"),
            self.account_xpub
        )
    }
}

fn ser_xonly<S: serde::Serializer>(k: &XOnlyPublicKey, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&k.to_string())
}

fn ser_display<T: std::fmt::Display, S: serde::Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(v)
}

/// The Payout Administrator account: `m/86'/0'/73'` on mainnet, `m/86'/1'/73'` elsewhere (the
/// Ledger Bitcoin Test app only derives coin type 1'). This is also the Admin Wallet account, so
/// `account/0/0` — the admin key in `params.toml` — is the Admin Wallet's first receive key.
pub fn account_path(network: Network) -> DerivationPath {
    let coin = if network == Network::Bitcoin { 0 } else { 1 };
    DerivationPath::from_str(&format!("m/86'/{coin}'/73'")).expect("static path")
}

/// The admin key path (`account/0/0`). It is also the Payout Administrator's Admin ID path.
pub fn admin_key_path(network: Network) -> DerivationPath {
    account_path(network).extend([
        ChildNumber::Normal { index: 0 },
        ChildNumber::Normal { index: 0 },
    ])
}

/// `xpub/0/0` as an x-only key.
pub fn xonly_at_0_0(xpub: &Xpub) -> Result<XOnlyPublicKey, bitcoin::bip32::Error> {
    let secp = bitcoin::secp256k1::Secp256k1::verification_only();
    let child = xpub.derive_pub(
        &secp,
        &[
            ChildNumber::Normal { index: 0 },
            ChildNumber::Normal { index: 0 },
        ],
    )?;
    Ok(child.public_key.x_only_public_key().0)
}

// ── Roster file ───────────────────────────────────────────────────────────────

/// The signer roster file (TOML):
///
/// ```toml
/// network = "regtest"
///
/// [[admins]]
/// label = "Admin A"
/// fingerprint = "f5acc2fd"
/// xpub = "tpubDC..."   # account m/86'/1'/73' (m/86'/0'/73' on mainnet)
/// ```
///
/// Admins are listed in the order of `[keys.admin].pubkeys`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerRoster {
    pub network: Option<String>,
    pub admins: Vec<RosterEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterEntry {
    pub label: String,
    pub fingerprint: String,
    pub xpub: String,
}

impl SignerRoster {
    pub fn from_toml(text: &str) -> Result<Self, DeploymentError> {
        toml::from_str(text).map_err(|e| DeploymentError::Roster(e.to_string()))
    }
}

// ── Deployment ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Deployment {
    pub network: Network,
    pub admin_keys: Vec<XOnlyPublicKey>,
    pub threshold: usize,
    pub covenants: Vec<Covenant>,
    pub operators: Vec<OperatorInfo>,
    /// Blocks after a claim until its uncontested payout (bridge `contest_timelock`).
    pub contest_timelock: u16,
    /// Present once [`Self::with_roster`] succeeded.
    pub signers: Option<Vec<AdminSigner>>,
    /// Non-fatal findings to show the user.
    pub warnings: Vec<String>,
}

impl Deployment {
    /// Parses `params.toml` with the bridge's own parser and derives every covenant.
    pub fn from_params_toml(text: &str) -> Result<Self, DeploymentError> {
        let params: Params =
            toml::from_str(text).map_err(|e| DeploymentError::Params(e.to_string()))?;
        Self::from_params(&params)
    }

    pub fn from_params(params: &Params) -> Result<Self, DeploymentError> {
        let network = params.network;
        let admin_keys = params.keys.admin.pubkeys.clone();
        let threshold = params.keys.admin.threshold;
        let mut warnings = Vec::new();
        if admin_keys.len() > LEDGER_MAX_MULTI_A_KEYS {
            warnings.push(format!(
                "A Ledger cannot sign multi_a with more than {LEDGER_MAX_MULTI_A_KEYS} keys; this \
                 admin set has {}.",
                admin_keys.len()
            ));
        }

        let schedule = &params.keys.operators;
        let operators: Vec<OperatorInfo> = schedule
            .iter()
            .map(|op| OperatorInfo {
                index: op.index(),
                covenant_key: op.covenant_key().to_string(),
                activation_height: op.activation_height(),
                deactivation_height: op.deactivation_height(),
            })
            .collect();

        // Every height at which the active set can change.
        let events: BTreeSet<_> = operators
            .iter()
            .flat_map(|op| std::iter::once(op.activation_height).chain(op.deactivation_height))
            .collect();
        let mut covenants: Vec<Covenant> = Vec::new();
        for height in events {
            let active: Vec<_> = schedule.active_at(height).collect();
            if active.is_empty() {
                continue;
            }
            let keys: Vec<PublicKey> = active.iter().map(|o| o.covenant_public_key()).collect();
            let n_of_n: PublicKey = KeyAggContext::new(keys)
                .map_err(|e| DeploymentError::Params(format!("N/N key aggregation failed: {e}")))?
                .aggregated_pubkey();
            if covenants.iter().any(|c| c.n_of_n == n_of_n) {
                continue;
            }
            let connector =
                ClaimAdminPayoutConnector::new(network, n_of_n, admin_keys.clone(), threshold);
            let connector_script = connector.script_pubkey();
            covenants.push(Covenant {
                index: covenants.len(),
                n_of_n,
                n_of_n_xonly: n_of_n.x_only_public_key().0,
                operator_indices: active.iter().map(|o| o.index()).collect(),
                from_height: height,
                connector,
                connector_script,
            });
        }
        if covenants.is_empty() {
            return Err(DeploymentError::Params(
                "the operator schedule never has an active operator".into(),
            ));
        }

        Ok(Self {
            network,
            admin_keys,
            threshold,
            covenants,
            operators,
            contest_timelock: params.protocol.contest_timelock,
            signers: None,
            warnings,
        })
    }

    /// Attaches the signer roster after checking it against `params.toml`.
    pub fn with_roster(mut self, roster: &SignerRoster) -> Result<Self, DeploymentError> {
        let err = DeploymentError::Roster;
        if let Some(net) = &roster.network {
            let parsed = Network::from_core_arg(net)
                .or_else(|_| Network::from_str(net))
                .map_err(|_| err(format!("unknown network \"{net}\"")))?;
            if parsed != self.network {
                return Err(err(format!(
                    "roster is for {parsed}, params.toml is for {}",
                    self.network
                )));
            }
        }
        if roster.admins.len() != self.admin_keys.len() {
            return Err(err(format!(
                "lists {} admins, params.toml has {}",
                roster.admins.len(),
                self.admin_keys.len()
            )));
        }
        let want_kind = NetworkKind::from(self.network);
        let mut fingerprints = BTreeSet::new();
        let mut signers = Vec::with_capacity(roster.admins.len());
        for (index, (entry, &expected)) in roster.admins.iter().zip(&self.admin_keys).enumerate() {
            let at = format!("admin {index} ({})", entry.label);
            if entry.label.trim().is_empty() {
                return Err(err(format!("admin {index}: label is required")));
            }
            let fingerprint = Fingerprint::from_str(entry.fingerprint.trim())
                .map_err(|e| err(format!("{at}: fingerprint: {e}")))?;
            let xpub =
                Xpub::from_str(entry.xpub.trim()).map_err(|e| err(format!("{at}: xpub: {e}")))?;
            if xpub.network != want_kind {
                return Err(err(format!(
                    "{at}: xpub is for the wrong network kind (expected {})",
                    if want_kind == NetworkKind::Main {
                        "xpub"
                    } else {
                        "tpub"
                    }
                )));
            }
            if xpub.depth != 3 {
                return Err(err(format!(
                    "{at}: xpub must be the account key at {} (depth 3), got depth {}",
                    account_path(self.network),
                    xpub.depth
                )));
            }
            let derived = xonly_at_0_0(&xpub).map_err(|e| err(format!("{at}: {e}")))?;
            if derived != expected {
                return Err(err(format!(
                    "{at}: xpub/0/0 is {derived}, but params.toml lists {expected} at this position"
                )));
            }
            if !fingerprints.insert(fingerprint) {
                self.warnings.push(format!(
                    "Two admins share master fingerprint {fingerprint}: they would be the same \
                     signing device."
                ));
            }
            signers.push(AdminSigner {
                index,
                label: entry.label.trim().to_string(),
                xonly: expected,
                fingerprint,
                account_xpub: xpub,
            });
        }
        self.signers = Some(signers);
        Ok(self)
    }

    pub fn load(params_toml: &str, roster_toml: Option<&str>) -> Result<Self, DeploymentError> {
        let dep = Self::from_params_toml(params_toml)?;
        match roster_toml {
            Some(r) => dep.with_roster(&SignerRoster::from_toml(r)?),
            None => Ok(dep),
        }
    }

    pub fn signers(&self) -> Result<&[AdminSigner], DeploymentError> {
        self.signers.as_deref().ok_or_else(|| {
            DeploymentError::Roster("no signer roster is loaded (the account xpubs)".into())
        })
    }

    pub fn admin_index(&self, key: &XOnlyPublicKey) -> Option<usize> {
        self.admin_keys.iter().position(|k| k == key)
    }

    pub fn covenant_for_script(&self, script: &ScriptBuf) -> Option<&Covenant> {
        self.covenants
            .iter()
            .find(|c| &c.connector_script == script)
    }

    pub fn covenant_for_n_of_n_xonly(&self, key: &XOnlyPublicKey) -> Option<&Covenant> {
        self.covenants.iter().find(|c| &c.n_of_n_xonly == key)
    }

    pub fn operator_index_for_key(&self, key: &XOnlyPublicKey) -> Option<u32> {
        let k = key.to_string();
        self.operators
            .iter()
            .find(|o| o.covenant_key == k)
            .map(|o| o.index)
    }

    /// Identifies the admin set and covenants. The coordinator and every desktop compute it from
    /// their own `params.toml`; the UI warns when they differ.
    pub fn fingerprint(&self) -> String {
        let mut canon = format!("{}|{}|", self.network, self.threshold);
        for k in &self.admin_keys {
            canon.push_str(&k.to_string());
            canon.push(',');
        }
        canon.push('|');
        for c in &self.covenants {
            canon.push_str(&hex::encode(c.n_of_n.serialize()));
            canon.push(',');
        }
        sha256::Hash::hash(canon.as_bytes()).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use strata_bridge_key_deriv::key_agg::derive_musig_0_0;

    #[test]
    fn loads_the_bridge_docker_params() {
        let dep = Deployment::from_params_toml(DOCKER_PARAMS).unwrap();
        assert_eq!(dep.network, Network::Regtest);
        assert_eq!(dep.threshold, 2);
        assert_eq!(dep.admin_keys.len(), 3);
        assert_eq!(dep.covenants.len(), 1);
        assert_eq!(dep.covenants[0].operator_indices, vec![0, 1, 2]);
        assert_eq!(dep.contest_timelock, 144);
    }

    #[test]
    fn synthetic_xpub_0_0_is_the_bridge_internal_key() {
        let dep = Deployment::from_params_toml(DOCKER_PARAMS).unwrap();
        let cov = &dep.covenants[0];
        let secp = bitcoin::secp256k1::Secp256k1::verification_only();
        let child = cov
            .synthetic_xpub(dep.network)
            .derive_pub(
                &secp,
                &[
                    ChildNumber::Normal { index: 0 },
                    ChildNumber::Normal { index: 0 },
                ],
            )
            .unwrap();
        let bridge = derive_musig_0_0(cov.n_of_n).unwrap().0;
        assert_eq!(child.public_key, bridge);
        assert_eq!(cov.connector.internal_key(), bridge.x_only_public_key().0);
    }

    #[test]
    fn operator_schedule_changes_create_covenants() {
        let fx = Fixture::new(3, 2);
        let toml =
            fx.params_toml_with_schedule(&[(0, 101, None), (1, 101, Some(300)), (2, 200, None)]);
        let dep = Deployment::from_params_toml(&toml).unwrap();
        let sets: Vec<_> = dep
            .covenants
            .iter()
            .map(|c| c.operator_indices.clone())
            .collect();
        assert_eq!(sets, vec![vec![0, 1], vec![0, 1, 2], vec![0, 2]]);
        let heights: Vec<_> = dep.covenants.iter().map(|c| c.from_height).collect();
        assert_eq!(heights, vec![101, 200, 300]);
    }

    #[test]
    fn roster_must_match_params_keys() {
        let fx = Fixture::new(3, 2);
        let dep = Deployment::load(&fx.params_toml(), Some(&fx.roster_toml())).unwrap();
        let signers = dep.signers().unwrap();
        assert_eq!(signers.len(), 3);
        assert_eq!(signers[1].xonly, fx.admin_xonly(1));

        // Swapping two admins puts each xpub at the wrong position.
        let swapped = fx.roster_toml_with_order(&[1, 0, 2]);
        let e = Deployment::load(&fx.params_toml(), Some(&swapped)).unwrap_err();
        assert!(e.to_string().contains("xpub/0/0"), "{e}");

        // A roster with an admin missing.
        let short = fx.roster_toml_with_order(&[0, 1]);
        let e = Deployment::load(&fx.params_toml(), Some(&short)).unwrap_err();
        assert!(e.to_string().contains("lists 2 admins"), "{e}");
    }

    #[test]
    fn roster_rejects_wrong_network() {
        let fx = Fixture::new(2, 2);
        let roster = fx
            .roster_toml()
            .replace("network = \"regtest\"", "network = \"bitcoin\"");
        let e = Deployment::load(&fx.params_toml(), Some(&roster)).unwrap_err();
        assert!(e.to_string().contains("roster is for"), "{e}");
    }

    #[test]
    fn policy_key_has_the_account_origin() {
        let fx = Fixture::new(2, 1);
        let dep = Deployment::load(&fx.params_toml(), Some(&fx.roster_toml())).unwrap();
        let k = dep.signers().unwrap()[0].policy_key(dep.network);
        assert!(
            k.starts_with(&format!("[{}/86'/1'/73']tpub", fx.fingerprint(0))),
            "{k}"
        );
    }

    #[test]
    fn fingerprint_changes_with_the_admin_set() {
        let a = Deployment::from_params_toml(&Fixture::new(3, 2).params_toml()).unwrap();
        let b = Deployment::from_params_toml(&Fixture::new(3, 3).params_toml()).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
    }
}
