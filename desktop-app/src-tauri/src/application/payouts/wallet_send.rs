use super::{Floors, PayoutContext, WalletCoin};
pub(super) fn address_floors(ctx: &PayoutContext, coins: &[WalletCoin]) -> Floors {
    ctx.used_addresses
        .seen(&wallet_key(ctx), coins.iter().map(|c| (c.branch, c.index)))
}
pub(super) fn wallet_key(ctx: &PayoutContext) -> String {
    format!("{}:{}", ctx.dep.network, ctx.me.fingerprint)
}
