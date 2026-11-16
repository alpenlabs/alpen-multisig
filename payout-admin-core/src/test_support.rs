//! Deterministic deployment fixtures.
use crate::deployment::{account_path, Deployment};
use bitcoin::{
    bip32::{Xpriv, Xpub},
    secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey},
    Network,
};
use std::str::FromStr;

pub const DOCKER_PARAMS: &str = r#"network = "regtest"
genesis_height = 101

[keys]

[keys.admin]
pubkeys = [
  "b49092f76d06f8002e0b7f1c63b5058db23fd4465b4f6954b53e1f352a04754d",
  "1e62d54af30569fd7269c14b6766f74d85ea00c911c4e1a423d4ba2ae4c34dc4",
  "a4d869ccd09c470f8f86d3f1b0997fa2695933aaea001875b9db145ae9c1f4ba",
]
threshold = 2

[[keys.operators]]
index = 0
covenant_key = "b49092f76d06f8002e0b7f1c63b5058db23fd4465b4f6954b53e1f352a04754d"
p2p_key = "0de7729dcbeb5069136ee4bff1c4f2fd822fe8fbc9b518df434d4f0c6312d8f5"
payout_descriptor = "0494047994e5ac5dabf9737a176f0c79eeb9f09c9749a087a138ddb1ff3caceb2f"
activation_height = 101

[[keys.operators]]
index = 1
covenant_key = "1e62d54af30569fd7269c14b6766f74d85ea00c911c4e1a423d4ba2ae4c34dc4"
p2p_key = "255ab0da6d468a22910a7cf54021763417c63c28bbafd4e2359daf103bb61e9d"
payout_descriptor = "044f935f6e983bd4bca4e512cc1a5f50137e3b4a03e20c90f60ac7387a88c2e114"
activation_height = 101

[[keys.operators]]
index = 2
covenant_key = "a4d869ccd09c470f8f86d3f1b0997fa2695933aaea001875b9db145ae9c1f4ba"
p2p_key = "aeabd3a377c160590a0927bca0cb315eaebf5acc87476fbb317c6260b6f123c6"
payout_descriptor = "04b63fea4640fbbf630e2bd949a588b63426bb2ea2771ce5b2caf44dbfa90d6cd3"
activation_height = 101

[protocol]
deposit_index_offset = 0
bury_depth = 2
magic_bytes = "ALPN"
deposit_amount = 1_000_000_000
stake_amount = 100_000_000
operator_fee = 10_000_000
sweep_fee_rate = 10
recovery_delay = 1_008
contest_timelock = 144
proof_timelock = 144
ack_timelock = 432
nack_timelock = 144
contested_payout_timelock = 1_008
unstaking_timelock = 2_016
"#;

const P2P_KEYS: [&str; 3] = [
    "0de7729dcbeb5069136ee4bff1c4f2fd822fe8fbc9b518df434d4f0c6312d8f5",
    "255ab0da6d468a22910a7cf54021763417c63c28bbafd4e2359daf103bb61e9d",
    "aeabd3a377c160590a0927bca0cb315eaebf5acc87476fbb317c6260b6f123c6",
];
const PAYOUT_DESCS: [&str; 3] = [
    "0494047994e5ac5dabf9737a176f0c79eeb9f09c9749a087a138ddb1ff3caceb2f",
    "044f935f6e983bd4bca4e512cc1a5f50137e3b4a03e20c90f60ac7387a88c2e114",
    "04b63fea4640fbbf630e2bd949a588b63426bb2ea2771ce5b2caf44dbfa90d6cd3",
];

pub struct Fixture {
    pub network: Network,
    pub threshold: usize,
    pub admin_masters: Vec<Xpriv>,
    pub operator_keys: Vec<SecretKey>,
}

fn sk(byte: u8) -> SecretKey {
    SecretKey::from_slice(&[byte; 32]).unwrap()
}

impl Fixture {
    pub fn new(n_admins: usize, threshold: usize) -> Self {
        let network = Network::Regtest;
        let admin_masters = (0..n_admins)
            .map(|i| Xpriv::new_master(network, &[i as u8 + 1; 32]).unwrap())
            .collect();
        let operator_keys = (0..3).map(|i| sk(100 + i as u8)).collect();
        Self {
            network,
            threshold,
            admin_masters,
            operator_keys,
        }
    }

    pub fn account_xpriv(&self, i: usize) -> Xpriv {
        let secp = Secp256k1::new();
        self.admin_masters[i]
            .derive_priv(&secp, &account_path(self.network))
            .unwrap()
    }

    pub fn account_xpub(&self, i: usize) -> Xpub {
        Xpub::from_priv(&Secp256k1::new(), &self.account_xpriv(i))
    }

    pub fn fingerprint(&self, i: usize) -> bitcoin::bip32::Fingerprint {
        self.admin_masters[i].fingerprint(&Secp256k1::new())
    }

    /// The admin's signing key: account/0/0.
    pub fn admin_keypair(&self, i: usize) -> Keypair {
        let secp = Secp256k1::new();
        let k = self
            .account_xpriv(i)
            .derive_priv(
                &secp,
                &bitcoin::bip32::DerivationPath::from_str("m/0/0").unwrap(),
            )
            .unwrap();
        Keypair::from_secret_key(&secp, &k.private_key)
    }

    pub fn admin_xonly(&self, i: usize) -> XOnlyPublicKey {
        self.admin_keypair(i).x_only_public_key().0
    }

    pub fn operator_xonly(&self, i: usize) -> XOnlyPublicKey {
        self.operator_keys[i].x_only_public_key(&Secp256k1::new()).0
    }

    pub fn foreign_n_of_n(&self) -> PublicKey {
        PublicKey::from_secret_key(&Secp256k1::new(), &sk(77))
    }

    pub fn params_toml(&self) -> String {
        self.params_toml_with_schedule(&[(0, 101, None), (1, 101, None), (2, 101, None)])
    }

    pub fn params_toml_with_schedule(&self, ops: &[(usize, u64, Option<u64>)]) -> String {
        let mut s = format!(
            "network = \"regtest\"\ngenesis_height = 101\n\n[keys.admin]\npubkeys = [{}]\nthreshold = {}\n\n",
            (0..self.admin_masters.len())
                .map(|i| format!("\"{}\"", self.admin_xonly(i)))
                .collect::<Vec<_>>()
                .join(", "),
            self.threshold
        );
        for &(i, act, deact) in ops {
            s.push_str(&format!(
                "[[keys.operators]]\nindex = {i}\ncovenant_key = \"{}\"\np2p_key = \"{}\"\npayout_descriptor = \"{}\"\nactivation_height = {act}\n",
                self.operator_xonly(i),
                P2P_KEYS[i],
                PAYOUT_DESCS[i]
            ));
            if let Some(d) = deact {
                s.push_str(&format!("deactivation_height = {d}\n"));
            }
            s.push('\n');
        }
        let proto = DOCKER_PARAMS.split("[protocol]").nth(1).unwrap();
        s.push_str("[protocol]");
        s.push_str(proto);
        s
    }

    pub fn roster_toml(&self) -> String {
        let order: Vec<usize> = (0..self.admin_masters.len()).collect();
        self.roster_toml_with_order(&order)
    }

    pub fn roster_toml_with_order(&self, order: &[usize]) -> String {
        let mut s = String::from("network = \"regtest\"\n\n");
        for &i in order {
            s.push_str(&format!(
                "[[admins]]\nlabel = \"Admin {i}\"\nfingerprint = \"{}\"\nxpub = \"{}\"\n\n",
                self.fingerprint(i),
                self.account_xpub(i)
            ));
        }
        s
    }

    pub fn deployment(&self) -> Deployment {
        Deployment::load(&self.params_toml(), Some(&self.roster_toml())).unwrap()
    }
}
