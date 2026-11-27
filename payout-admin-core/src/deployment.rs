//! Trusted payout signer roster and verified ASM operator history.

use std::{collections::BTreeSet, str::FromStr};

use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
    hashes::{sha256, Hash},
    secp256k1::{PublicKey, XOnlyPublicKey},
    Network, NetworkKind, ScriptBuf, TxOut,
};
use musig2::KeyAggContext;
use serde::{Deserialize, Serialize};
use strata_asm_proto_bridge_state::operator::{build_nn_script, OperatorTable};
use strata_bridge_connectors::{prelude::ClaimAdminPayoutConnector, Connector};

/// BIP-328 chain code of MuSig2 synthetic xpubs (the bridge's `key-deriv` crate uses the same).
pub const BIP328_CHAIN_CODE: [u8; 32] = [
    0x86, 0x80, 0x87, 0xca, 0x02, 0xa6, 0xf9, 0x74, 0xc4, 0x59, 0x89, 0x24, 0xc3, 0x6b, 0x57, 0x76,
    0x2d, 0x32, 0xcb, 0x45, 0x71, 0x71, 0x67, 0xe3, 0x00, 0x62, 0x2c, 0x71, 0x67, 0xe3, 0x89, 0x65,
];

/// Ledger Bitcoin app limit on `multi_a` keys.
pub const LEDGER_MAX_MULTI_A_KEYS: usize = 16;

/// Identifies whether deployment setup failed on ASM history or signer roster data.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeploymentError {
    #[error("ASM N/N history: {0}")]
    History(String),
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
    /// Position in the signer roster.
    index: usize,
    label: String,
    /// The admin signing key (`account_xpub/0/0`).
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
/// `account/0/0` — the admin signing key — is the Admin Wallet's first receive key.
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
/// threshold = 1
///
/// [[admins]]
/// label = "Admin A"
/// fingerprint = "f5acc2fd"
/// xpub = "tpubDC..."   # account m/86'/1'/73' (m/86'/0'/73' on mainnet)
/// ```
///
/// Admins are listed in connector script order.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerRoster {
    #[serde(deserialize_with = "roster_network")]
    network: Network,
    threshold: usize,
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

fn roster_network<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Network, D::Error> {
    let network = Network::deserialize(deserializer)?;
    match network {
        Network::Bitcoin | Network::Testnet | Network::Signet | Network::Regtest => Ok(network),
        _ => Err(serde::de::Error::custom(format!(
            "unsupported roster network {network}"
        ))),
    }
}

impl SignerRoster {
    /// Decodes the signer file. The roster sets the network, threshold and admin key order.
    pub fn from_toml(text: &str) -> Result<Self, DeploymentError> {
        toml::from_str(text).map_err(|error| DeploymentError::Roster(error.to_string()))
    }
}

// ── Deployment ────────────────────────────────────────────────────────────────

/// Trusted signer roster and connectors derived from ASM history.
#[derive(Debug, Clone)]
pub struct Deployment {
    admin_keys: Vec<XOnlyPublicKey>,
    threshold: usize,
    covenants: Vec<Covenant>,
    signers: Vec<AdminSigner>,
    /// Non-fatal findings to show the user.
    warnings: Vec<String>,
}

impl Deployment {
    /// Nonfatal findings from loading the signer roster.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The x-only signing keys in connector script order.
    pub fn admin_keys(&self) -> &[XOnlyPublicKey] {
        &self.admin_keys
    }

    /// The covenant for the latest ASM history entry.
    pub fn covenant(&self) -> &Covenant {
        self.covenants
            .last()
            .expect("validated nonempty ASM history")
    }

    /// Every covenant in ASM's N/N history, oldest first.
    pub fn covenants(&self) -> &[Covenant] {
        &self.covenants
    }

    /// The Bitcoin network configured for the payout connector.
    pub fn network(&self) -> Network {
        self.covenant().connector.network()
    }

    /// Number of admin signatures required per connector input.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Constructs connectors for every verified entry using roster order for the admin leaf.
    pub fn load(roster: &SignerRoster, operators: &OperatorTable) -> Result<Self, DeploymentError> {
        let roster_error = DeploymentError::Roster;
        let network = roster.network;
        let threshold = roster.threshold;
        let mut warnings = Vec::new();
        let mut admin_keys = Vec::new();

        if threshold == 0 || roster.admins.len() < threshold {
            return Err(roster_error(
                "threshold must be between 1 and the number of admins".into(),
            ));
        }

        if LEDGER_MAX_MULTI_A_KEYS < roster.admins.len() {
            warnings.push(format!(
                "A Ledger cannot sign multi_a with more than {LEDGER_MAX_MULTI_A_KEYS} keys; this \
                 admin set has {}.",
                roster.admins.len()
            ));
        }

        let want_kind = NetworkKind::from(network);
        let mut fingerprints = BTreeSet::new();
        let mut signers = Vec::with_capacity(roster.admins.len());
        for (index, entry) in roster.admins.iter().enumerate() {
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
                    account_path(network),
                    xpub.depth
                )));
            }

            let derived =
                xonly_at_0_0(&xpub).map_err(|error| roster_error(format!("{at}: {error}")))?;

            if admin_keys.contains(&derived) {
                return Err(roster_error(format!("{at}: duplicate admin key {derived}")));
            }

            admin_keys.push(derived);

            if !fingerprints.insert(fingerprint) {
                warnings.push(format!(
                    "Two admins share master fingerprint {fingerprint}: they would be the same \
                     signing device."
                ));
            }

            signers.push(AdminSigner {
                index,
                label: entry.label.trim().to_string(),
                xonly: derived,
                fingerprint,
                account_xpub: xpub,
            });
        }
        let covenants = Self::history_keys(operators)?
            .into_iter()
            .map(|n_of_n| {
                let connector =
                    ClaimAdminPayoutConnector::new(network, n_of_n, admin_keys.clone(), threshold);
                Covenant { n_of_n, connector }
            })
            .collect();

        Ok(Self {
            admin_keys,
            threshold,
            signers,
            warnings,
            covenants,
        })
    }

    /// Rebuilds the full N/N key of every ASM history entry and checks it against the entry's
    /// recorded script.
    pub fn history_keys(operators: &OperatorTable) -> Result<Vec<PublicKey>, DeploymentError> {
        let history_error = DeploymentError::History;
        let mut keys_in_history = Vec::new();

        for entry in operators.historical_nn_scripts() {
            let keys = entry
                .operators()
                .active_indices()
                .map(|index| {
                    operators
                        .get_operator(index)
                        .map(|operator| PublicKey::from(*operator.musig2_pk()))
                        .ok_or_else(|| history_error(format!("unknown operator index {index}")))
                })
                .collect::<Result<Vec<_>, _>>()?;

            if keys.is_empty() {
                return Err(history_error("empty operator bitmap".into()));
            }

            let n_of_n: PublicKey = KeyAggContext::new(keys)
                .map_err(|error| history_error(error.to_string()))?
                .aggregated_pubkey();

            if build_nn_script(&n_of_n.x_only_public_key().0.into()).inner() != entry.script() {
                return Err(history_error(
                    "operator bitmap does not match the recorded script".into(),
                ));
            }

            keys_in_history.push(n_of_n);
        }

        if keys_in_history.is_empty() {
            return Err(history_error("empty history".into()));
        }

        Ok(keys_in_history)
    }

    pub fn signers(&self) -> &[AdminSigner] {
        &self.signers
    }

    /// Finds the key's zero-based position in the deployment's ordered admin set.
    pub fn admin_index(&self, key: &XOnlyPublicKey) -> Option<usize> {
        self.admin_keys
            .iter()
            .position(|admin_key| admin_key == key)
    }

    /// Hashes the network, threshold, ordered admin keys, and full N/N key history with SHA-256.
    /// The desktop returns the hex digest when it loads the deployment.
    pub fn digest(&self) -> sha256::Hash {
        let mut canonical = format!("{}|{}|", self.network(), self.threshold);
        for admin_key in &self.admin_keys {
            canonical.push_str(&admin_key.to_string());
            canonical.push(',');
        }
        canonical.push('|');
        for covenant in &self.covenants {
            canonical.push_str(&hex::encode(covenant.n_of_n.serialize()));
        }

        sha256::Hash::hash(canonical.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use strata_bridge_key_deriv::key_agg::derive_musig_0_0;

    #[test]
    fn asm_history_matches_full_bridge_aggregate_and_script() {
        use strata_asm_proto_bridge_state::operator::{build_nn_script, OperatorTable};
        use strata_crypto::EvenPublicKey;

        let params: strata_bridge_common::params::Params = toml::from_str(DOCKER_PARAMS).unwrap();
        let schedule = &params.keys.operators;
        let operators: Vec<_> = schedule
            .active_at(101)
            .map(|operator| EvenPublicKey::from(operator.covenant_public_key()))
            .collect();
        let mut table = OperatorTable::from_operator_list(&operators);
        let entry = table.historical_nn_scripts().next().unwrap();
        let selected = entry
            .operators()
            .active_indices()
            .map(|index| PublicKey::from(*table.get_operator(index).unwrap().musig2_pk()));
        let full_key: PublicKey = KeyAggContext::new(selected).unwrap().aggregated_pubkey();
        let bridge_key: PublicKey = KeyAggContext::new(
            schedule
                .active_at(101)
                .map(|operator| operator.covenant_public_key()),
        )
        .unwrap()
        .aggregated_pubkey();

        assert_eq!(full_key, bridge_key);
        let deployment = Deployment::load(
            &SignerRoster::from_toml(&Fixture::new(3, 2).roster_toml()).unwrap(),
            &table,
        )
        .unwrap();
        assert_eq!(deployment.covenant().n_of_n(), bridge_key);
        assert_eq!(
            build_nn_script(&full_key.x_only_public_key().0.into()).inner(),
            entry.script()
        );

        // Remove the middle index, leaving a gap in the active bitmap.
        table.apply_membership_changes(&[], &[1]);
        let remaining: PublicKey = KeyAggContext::new(
            schedule
                .active_at(101)
                .filter(|operator| operator.index() != 1)
                .map(|operator| operator.covenant_public_key()),
        )
        .unwrap()
        .aggregated_pubkey();
        assert_eq!(
            Deployment::history_keys(&table).unwrap(),
            [bridge_key, remaining]
        );
    }

    #[test]
    fn synthetic_xpub_0_0_is_the_bridge_internal_key() {
        let deployment = Fixture::new(3, 2).deployment();
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
    fn mismatched_asm_script_is_rejected() {
        use ssz::{Decode, Encode};
        let table = Fixture::new(3, 2).operator_table();
        let script = table
            .historical_nn_scripts()
            .next()
            .unwrap()
            .script()
            .as_bytes();
        let mut bytes = table.as_ssz_bytes();
        let offset = bytes
            .windows(script.len())
            .position(|window| window == script)
            .unwrap();
        bytes[offset] ^= 1;
        let inconsistent = OperatorTable::from_ssz_bytes(&bytes).unwrap();
        assert!(Deployment::history_keys(&inconsistent)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
    }

    #[test]
    fn roster_requires_network_and_valid_threshold() {
        let fixture = Fixture::new(3, 2);
        let text = fixture.roster_toml();
        for required in ["network", "threshold"] {
            let missing = text
                .lines()
                .filter(|line| !line.starts_with(required))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(SignerRoster::from_toml(&missing).is_err());
        }
        for threshold in [0, 4] {
            let roster = SignerRoster::from_toml(
                &text.replace("threshold = 2", &format!("threshold = {threshold}")),
            )
            .unwrap();
            assert!(Deployment::load(&roster, &fixture.operator_table()).is_err());
        }
    }

    #[test]
    fn xpubs_must_match_the_roster_network() {
        let fixture = Fixture::new(3, 2);
        // The fixture's xpubs are testnet keys (tpub); a mainnet roster must refuse them.
        let mainnet = SignerRoster::from_toml(
            &fixture
                .roster_toml()
                .replace("network = \"regtest\"", "network = \"bitcoin\""),
        )
        .unwrap();
        assert!(Deployment::load(&mainnet, &fixture.operator_table())
            .unwrap_err()
            .to_string()
            .contains("wrong network kind"));
    }

    #[test]
    fn duplicate_keys_are_refused_and_duplicate_fingerprints_warn() {
        let fixture = Fixture::new(3, 2);
        let operators = fixture.operator_table();
        let duplicate =
            SignerRoster::from_toml(&fixture.roster_toml_with_order(&[0, 1, 0])).unwrap();
        assert!(Deployment::load(&duplicate, &operators)
            .unwrap_err()
            .to_string()
            .contains("duplicate admin key"));
        let mut shared = SignerRoster::from_toml(&fixture.roster_toml()).unwrap();
        shared.admins[1].fingerprint = shared.admins[0].fingerprint;
        assert!(Deployment::load(&shared, &operators).unwrap().warnings()[0]
            .contains("share master fingerprint"));
    }

    #[test]
    fn roster_controls_admin_key_order() {
        let fixture = Fixture::new(3, 2);
        let roster = SignerRoster::from_toml(&fixture.roster_toml_with_order(&[1, 0, 2])).unwrap();
        let deployment = Deployment::load(&roster, &fixture.operator_table()).unwrap();
        assert_eq!(deployment.admin_keys()[0], fixture.admin_xonly(1));
    }

    #[test]
    fn digest_changes_with_the_admin_set() {
        let two_required = Fixture::new(3, 2).deployment();
        let three_required = Fixture::new(3, 3).deployment();
        assert_ne!(two_required.digest(), three_required.digest());
        assert_eq!(two_required.digest(), two_required.clone().digest());
    }
}
