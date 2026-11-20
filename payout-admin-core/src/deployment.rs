//! The trusted deployment: strata-bridge `params.toml` plus the Payout Administrator signer roster.
//!
//! `params.toml` is the bridge's consensus parameter file. From it come:
//!
//! - the admin multisig: `[keys.admin] pubkeys` (x-only, in order) and `threshold`;
//! - the operator schedule, whose latest activation or deactivation height selects the active
//!   operator set. Its aggregate N/N key and the admin set define the payout connector.
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

/// Identifies whether deployment setup failed on bridge parameters or signer roster data.
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
    /// Full N/N aggregate key (needed for the BIP-328 internal key of the connector).
    n_of_n: PublicKey,
    connector: ClaimAdminPayoutConnector,
}

impl Covenant {
    /// Returns the full N/N aggregate key.
    pub fn n_of_n(&self) -> PublicKey {
        self.n_of_n
    }

    /// The N/N key used in the bridge leaves.
    pub fn n_of_n_xonly(&self) -> XOnlyPublicKey {
        self.n_of_n.x_only_public_key().0
    }

    /// The bridge connector spent through the admin multisig leaf.
    pub fn connector(&self) -> &ClaimAdminPayoutConnector {
        &self.connector
    }

    /// The scriptPubKey expected at the claim's admin payout output.
    pub fn connector_script(&self) -> ScriptBuf {
        self.connector.script_pubkey()
    }

    pub(crate) const SHORT_ID_BYTES: usize = 3;

    /// Returns the script and value expected at a claim's admin payout output.
    pub fn connector_tx_out(&self) -> TxOut {
        self.connector.tx_out()
    }

    /// Short identifier shown to signers and used in Ledger policy names.
    pub fn short_id(&self) -> String {
        hex::encode(&self.n_of_n.serialize()[1..1 + Self::SHORT_ID_BYTES])
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

/// The account keys and master fingerprint for a roster entry or a connected signing device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSigner {
    /// Position in `[keys.admin].pubkeys`.
    index: usize,
    label: String,
    /// The admin key as in `params.toml` (`account_xpub/0/0`).
    #[serde(serialize_with = "ser_xonly")]
    xonly: XOnlyPublicKey,
    #[serde(serialize_with = "ser_display")]
    fingerprint: Fingerprint,
    /// Account xpub at [`account_path`].
    #[serde(serialize_with = "ser_display")]
    account_xpub: Xpub,
}

impl AdminSigner {
    /// Zero-based roster position, initialized to zero for an identity read directly from a device.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Returns the x-only admin key at account/0/0.
    pub fn xonly(&self) -> XOnlyPublicKey {
        self.xonly
    }

    /// Creates the signer identity read from a device before a roster is loaded.
    pub fn from_device(
        fingerprint: Fingerprint,
        account_xpub: Xpub,
    ) -> Result<Self, bitcoin::bip32::Error> {
        Ok(Self {
            index: 0,
            label: String::new(),
            xonly: xonly_at_0_0(&account_xpub)?,
            fingerprint,
            account_xpub,
        })
    }

    /// The account xpub from which the admin signing key and wallet addresses are derived.
    pub fn account_xpub(&self) -> Xpub {
        self.account_xpub
    }

    /// The BIP-32 master fingerprint used in the signer's key origin.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Full derivation path from the master key to the admin signing key.
    pub fn key_path(&self, network: Network) -> DerivationPath {
        admin_key_path(network)
    }
}

fn ser_xonly<S: serde::Serializer>(
    admin_key: &XOnlyPublicKey,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&admin_key.to_string())
}

fn ser_display<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
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
    #[serde(default, deserialize_with = "roster_network")]
    network: Option<Network>,
    admins: Vec<RosterEntry>,
}

/// One roster row supplying an admin label, master fingerprint, and account xpub.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterEntry {
    label: String,
    fingerprint: Fingerprint,
    xpub: Xpub,
}

fn roster_network<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Network>, D::Error> {
    let network = Network::deserialize(deserializer)?;
    match network {
        Network::Bitcoin | Network::Testnet | Network::Signet | Network::Regtest => {
            Ok(Some(network))
        }
        _ => Err(serde::de::Error::custom(format!(
            "unsupported roster network {network}"
        ))),
    }
}

impl SignerRoster {
    /// Decodes the signer file. Matching its entries to the deployment requires `Deployment::with_roster`.
    pub fn from_toml(text: &str) -> Result<Self, DeploymentError> {
        toml::from_str(text).map_err(|error| DeploymentError::Roster(error.to_string()))
    }
}

// ── Deployment ────────────────────────────────────────────────────────────────

/// Bridge parameters used to verify claims and build payouts, optionally paired with signer account keys.
#[derive(Debug, Clone)]
pub struct Deployment {
    nack_timelock: bitcoin::relative::Height,
    admin_keys: Vec<XOnlyPublicKey>,
    threshold: usize,
    /// Only the operator set active at the newest activation/deactivation height in
    /// params.toml is supported. Claims from earlier sets cannot be verified or blocked.
    covenant: Covenant,
    /// Present once [`Self::with_roster`] succeeded.
    signers: Option<Vec<AdminSigner>>,
    /// Non-fatal findings to show the user.
    warnings: Vec<String>,
}

impl Deployment {
    /// Nonfatal findings from loading the bridge parameters.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The x-only signing keys in connector script order.
    pub fn admin_keys(&self) -> &[XOnlyPublicKey] {
        &self.admin_keys
    }

    /// The covenant derived at the newest operator schedule height.
    pub fn covenant(&self) -> &Covenant {
        &self.covenant
    }

    /// Relative delay in blocks used to reconstruct the counterproof timeout leaf.
    pub fn nack_timelock(&self) -> bitcoin::relative::Height {
        self.nack_timelock
    }

    /// The Bitcoin network configured for the payout connector.
    pub fn network(&self) -> Network {
        self.covenant.connector.network()
    }

    /// Number of admin signatures required per connector input.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Parses `params.toml` with the bridge's own parser and derives the newest operator set.
    pub fn from_params_toml(text: &str) -> Result<Self, DeploymentError> {
        let params: Params =
            toml::from_str(text).map_err(|error| DeploymentError::Params(error.to_string()))?;

        Self::from_params(&params)
    }

    /// Builds the payout connector for the operators active at the newest scheduled height.
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
        let height = schedule
            .iter()
            .flat_map(|operator| {
                std::iter::once(operator.activation_height()).chain(operator.deactivation_height())
            })
            .max()
            .ok_or_else(|| DeploymentError::Params("empty operator schedule".into()))?;
        let active: Vec<_> = schedule.active_at(height).collect();

        if active.is_empty() {
            return Err(DeploymentError::Params(
                "no active operators at the newest schedule height".into(),
            ));
        }

        let keys: Vec<PublicKey> = active
            .iter()
            .map(|operator| operator.covenant_public_key())
            .collect();
        let n_of_n: PublicKey = KeyAggContext::new(keys)
            .map_err(|error| {
                DeploymentError::Params(format!("N/N key aggregation failed: {error}"))
            })?
            .aggregated_pubkey();
        let connector =
            ClaimAdminPayoutConnector::new(network, n_of_n, admin_keys.clone(), threshold);
        let covenant = Covenant { n_of_n, connector };

        Ok(Self {
            nack_timelock: bitcoin::relative::Height::from_height(params.protocol.nack_timelock),
            admin_keys,
            threshold,
            covenant,
            signers: None,
            warnings,
        })
    }

    /// Attaches the signer roster after checking it against `params.toml`.
    pub fn with_roster(mut self, roster: &SignerRoster) -> Result<Self, DeploymentError> {
        let roster_error = DeploymentError::Roster;

        if let Some(parsed) = roster.network {
            if parsed != self.network() {
                return Err(roster_error(format!(
                    "roster is for {parsed}, params.toml is for {}",
                    self.network()
                )));
            }
        }

        if roster.admins.len() != self.admin_keys.len() {
            return Err(roster_error(format!(
                "lists {} admins, params.toml has {}",
                roster.admins.len(),
                self.admin_keys.len()
            )));
        }

        let want_kind = NetworkKind::from(self.network());
        let mut fingerprints = BTreeSet::new();
        let mut signers = Vec::with_capacity(roster.admins.len());
        for (index, (entry, &expected)) in roster.admins.iter().zip(&self.admin_keys).enumerate() {
            let at = format!("admin {index} ({})", entry.label);

            if entry.label.trim().is_empty() {
                return Err(roster_error(format!("admin {index}: label is required")));
            }

            let fingerprint = entry.fingerprint;
            let xpub = entry.xpub;

            if xpub.network != want_kind {
                return Err(roster_error(format!(
                    "{at}: xpub is for the wrong network kind (expected {})",
                    if want_kind == NetworkKind::Main {
                        "xpub"
                    } else {
                        "tpub"
                    }
                )));
            }

            if xpub.depth != 3 {
                return Err(roster_error(format!(
                    "{at}: xpub must be the account key at {} (depth 3), got depth {}",
                    account_path(self.network()),
                    xpub.depth
                )));
            }

            let derived =
                xonly_at_0_0(&xpub).map_err(|error| roster_error(format!("{at}: {error}")))?;

            if derived != expected {
                return Err(roster_error(format!(
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

    /// Parses the bridge parameters and checks the signer roster against them when supplied.
    pub fn load(params_toml: &str, roster_toml: Option<&str>) -> Result<Self, DeploymentError> {
        let deployment = Self::from_params_toml(params_toml)?;
        match roster_toml {
            Some(roster_text) => deployment.with_roster(&SignerRoster::from_toml(roster_text)?),
            None => Ok(deployment),
        }
    }

    /// Returns the checked signers in admin-key order, or an error if no roster is loaded.
    pub fn signers(&self) -> Result<&[AdminSigner], DeploymentError> {
        self.signers.as_deref().ok_or_else(|| {
            DeploymentError::Roster("no signer roster is loaded (the account xpubs)".into())
        })
    }

    /// Finds the key's zero-based position in the deployment's ordered admin set.
    pub fn admin_index(&self, key: &XOnlyPublicKey) -> Option<usize> {
        self.admin_keys
            .iter()
            .position(|admin_key| admin_key == key)
    }

    /// Hashes the network, threshold, ordered admin keys, and full N/N key with SHA-256.
    /// The desktop returns the hex digest when it loads the deployment.
    pub fn digest(&self) -> sha256::Hash {
        let mut canonical = format!("{}|{}|", self.network(), self.threshold);
        for admin_key in &self.admin_keys {
            canonical.push_str(&admin_key.to_string());
            canonical.push(',');
        }
        canonical.push('|');
        canonical.push_str(&hex::encode(self.covenant.n_of_n.serialize()));

        sha256::Hash::hash(canonical.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use strata_bridge_key_deriv::key_agg::derive_musig_0_0;

    #[test]
    fn loads_the_bridge_docker_params() {
        let deployment = Deployment::from_params_toml(DOCKER_PARAMS).unwrap();
        assert_eq!(deployment.network(), Network::Regtest);
        assert_eq!(deployment.threshold(), 2);
        assert_eq!(deployment.admin_keys().len(), 3);
    }

    #[test]
    fn synthetic_xpub_0_0_is_the_bridge_internal_key() {
        let deployment = Deployment::from_params_toml(DOCKER_PARAMS).unwrap();
        let covenant = deployment.covenant();
        let secp = bitcoin::secp256k1::Secp256k1::verification_only();
        let child = covenant
            .synthetic_xpub(deployment.network())
            .derive_pub(
                &secp,
                &[
                    ChildNumber::Normal { index: 0 },
                    ChildNumber::Normal { index: 0 },
                ],
            )
            .unwrap();
        let bridge = derive_musig_0_0(covenant.n_of_n()).unwrap().0;
        assert_eq!(child.public_key, bridge);
        assert_eq!(
            covenant.connector().internal_key(),
            bridge.x_only_public_key().0
        );
    }

    #[test]
    fn roster_must_match_params_keys() {
        let fixture = Fixture::new(3, 2);
        let deployment =
            Deployment::load(&fixture.params_toml(), Some(&fixture.roster_toml())).unwrap();
        let signers = deployment.signers().unwrap();
        assert_eq!(signers.len(), 3);
        assert_eq!(signers[1].xonly(), fixture.admin_xonly(1));

        // Swapping two admins puts each xpub at the wrong position.
        let swapped = fixture.roster_toml_with_order(&[1, 0, 2]);
        let error = Deployment::load(&fixture.params_toml(), Some(&swapped)).unwrap_err();
        assert!(error.to_string().contains("xpub/0/0"), "{error}");

        // A roster with an admin missing.
        let short = fixture.roster_toml_with_order(&[0, 1]);
        let error = Deployment::load(&fixture.params_toml(), Some(&short)).unwrap_err();
        assert!(error.to_string().contains("lists 2 admins"), "{error}");
    }

    #[test]
    fn roster_rejects_wrong_network() {
        let fixture = Fixture::new(2, 2);
        let roster = fixture
            .roster_toml()
            .replace("network = \"regtest\"", "network = \"bitcoin\"");
        let error = Deployment::load(&fixture.params_toml(), Some(&roster)).unwrap_err();
        assert!(error.to_string().contains("roster is for"), "{error}");
    }

    #[test]
    fn digest_changes_with_the_admin_set() {
        let two_required = Deployment::from_params_toml(&Fixture::new(3, 2).params_toml()).unwrap();
        let three_required =
            Deployment::from_params_toml(&Fixture::new(3, 3).params_toml()).unwrap();
        assert_ne!(two_required.digest(), three_required.digest());
        assert_eq!(two_required.digest(), two_required.clone().digest());
    }
}
