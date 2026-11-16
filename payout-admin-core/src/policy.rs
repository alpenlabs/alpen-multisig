use crate::deployment::AdminSigner;
use bitcoin::Network;
use serde::Serialize;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySpec {
    /// At most 16 ASCII bytes (Ledger limit). Empty for the default policy.
    pub name: String,
    pub template: String,
    pub keys: Vec<String>,
}

pub fn admin_wallet_policy(signer: &AdminSigner, network: Network) -> PolicySpec {
    PolicySpec {
        name: String::new(),
        template: "tr(@0/**)".into(),
        keys: vec![signer.policy_key(network)],
    }
}
pub fn policy_descriptor(p: &PolicySpec, branch: u32) -> String {
    let mut out = p.template.clone();
    // Replace higher indices first so "@1" does not match inside "@10".
    for (i, key) in p.keys.iter().enumerate().rev() {
        out = out.replace(&format!("@{i}/**"), &format!("{key}/{branch}/*"));
    }
    out
}
