use super::{
    branch_index_of, child_key, policy, AdminSigner, Deployment, OutPoint, PayoutNode, ScriptBuf,
    Secp256k1,
};
/// An Admin Wallet coin, with where it sits in the account.
#[derive(Debug, Clone)]
pub struct WalletCoin {
    pub outpoint: OutPoint,
    pub value_sat: u64,
    pub script_pubkey: ScriptBuf,
    /// 0 receive, 1 change.
    pub branch: u32,
    pub index: u32,
    pub confirmations: u32,
}

pub(super) async fn admin_wallet_coins(
    dep: &Deployment,
    me: &AdminSigner,
    node: &dyn PayoutNode,
) -> Result<Vec<WalletCoin>, String> {
    let p = policy::admin_wallet_policy(me, dep.network);
    let descs = [
        policy::policy_descriptor(&p, 0),
        policy::policy_descriptor(&p, 1),
    ];
    let tip = node.tip().await?;
    let mut out = Vec::new();
    for c in node.scan(&descs).await? {
        let Some((branch, index)) = branch_index_of(&c.desc) else {
            continue;
        };
        // Only keys this admin's xpub derives (the descriptor could be inferred loosely).
        if branch > 1 || change_or_receive_script(me, branch, index)? != c.script_pubkey {
            continue;
        }
        if !node.unspent(&c.outpoint, true).await? {
            continue;
        }
        out.push(WalletCoin {
            outpoint: c.outpoint,
            value_sat: c.value_sat,
            script_pubkey: c.script_pubkey,
            branch,
            index,
            confirmations: if c.height == 0 {
                0
            } else {
                (tip + 1).saturating_sub(c.height) as u32
            },
        });
    }
    out.sort_by_key(|c| (c.branch, c.index, c.outpoint));
    Ok(out)
}
pub(super) fn change_or_receive_script(
    me: &AdminSigner,
    branch: u32,
    index: u32,
) -> Result<ScriptBuf, String> {
    Ok(ScriptBuf::new_p2tr(
        &Secp256k1::verification_only(),
        child_key(&me.account_xpub, branch, index)?,
        None,
    ))
}
