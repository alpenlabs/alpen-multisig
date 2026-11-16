use super::{
    address_floors, address_on_device, blocking, branch_index_of, child_key, payout_tx,
    pending_spends, policy, settle_sends, unix_now, used_scripts, wallet_rate, AdminSigner,
    CoordinatorPending, Deployment, FromStr, HashSet, OutPoint, PayoutContext, PayoutNode, Psbt,
    ScriptBuf, Secp256k1, Serialize, SCAN_RANGE,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletSendDto {
    pub txid: String,
    pub to_address: String,
    pub amount_sat: u64,
    pub fee_sat: u64,
    pub fee_rate: String,
    pub sent_at: i64,
    pub status: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletAddressDto {
    pub address: String,
    pub balance_sat: u64,
}

/// The Admin Wallet's confirmed coins, read from Bitcoin Core (`scantxoutset` over both branches
/// of the account). Coins already spent in the mempool (e.g. by an unconfirmed payout) are left out.
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminWalletCoinDto {
    pub outpoint: String,
    pub address: String,
    pub value_sat: u64,
    /// "receive" or "change".
    pub branch: &'static str,
    /// Already spent by a pending payout of this admin.
    pub reserved: bool,
    /// Spent by a transaction of the Admin Wallet panel that is not on the network yet.
    pub in_flight: bool,
}

/// The Admin Wallet as the payout flows see it: coins found on Bitcoin Core (`scantxoutset`, so
/// confirmed coins only) and the next receive address. The fee inputs of every payout come from
/// here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminWalletDto {
    pub unconfirmed_sat: i64,
    pub addresses: Vec<WalletAddressDto>,
    pub sends: Vec<WalletSendDto>,
    pub tip_height: u64,
    pub coins: Vec<AdminWalletCoinDto>,
    pub total_sat: u64,
    /// Neither reserved by a pending payout nor in flight: what new payouts can spend.
    pub available_sat: u64,
    pub receive_address: String,
    pub receive_index: u32,
    /// The receive address as the Ledger shows it (regtest: `tb1…`), when it differs.
    pub receive_on_device: Option<String>,
}

pub async fn admin_wallet(
    ctx: &PayoutContext,
    node: &dyn PayoutNode,
) -> Result<AdminWalletDto, String> {
    let _guard = ctx.sends.lock().await;
    let own = settle_sends(ctx, node).await?;
    let tip_height = node.tip().await?;
    let mut coins = admin_wallet_coins(&ctx.dep, &ctx.me, node).await?;
    let floors = address_floors(ctx, &coins);
    let spent: HashSet<_> = own
        .iter()
        .flat_map(|(s, _)| s.transaction.input.iter().map(|i| i.previous_output))
        .collect();
    coins.retain(|c| !spent.contains(&c.outpoint));
    *ctx.coins.lock().await = Some((std::time::Instant::now(), ctx.me.index, coins.clone()));
    let pending = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
    let past = ctx.coordinator.past().await.map_err(|e| e.to_string())?;
    share_pending_coins(ctx, &pending);
    let (_, reserved, _) = pending_spends(&ctx.dep, &pending);
    let in_flight = ctx.reservations.in_flight();
    let mut used = used_scripts(&coins, &pending, &past);
    for (send, _) in &own {
        used.extend(
            send.transaction
                .output
                .iter()
                .map(|o| o.script_pubkey.clone()),
        );
        used.extend(
            send.psbt
                .inputs
                .iter()
                .filter_map(|i| i.witness_utxo.as_ref().map(|o| o.script_pubkey.clone())),
        );
    }
    // First receive address that never held a coin and is not used by a payout (spec D4).
    let receive_index = (floors[0]..=SCAN_RANGE)
        .find(|&i| change_or_receive_script(&ctx.me, 0, i).is_ok_and(|s| !used.contains(&s)))
        .ok_or_else(|| {
            format!(
                "The first {SCAN_RANGE} receive addresses of your Admin Wallet are all used; this \
                 app does not look further."
            )
        })?;
    let script = change_or_receive_script(&ctx.me, 0, receive_index)?;
    let addr = |s: &ScriptBuf| {
        bitcoin::Address::from_script(s, ctx.dep.network)
            .map(|a| a.to_string())
            .unwrap_or_default()
    };
    let dtos: Vec<AdminWalletCoinDto> = coins
        .iter()
        .map(|c| AdminWalletCoinDto {
            outpoint: c.outpoint.to_string(),
            address: addr(&c.script_pubkey),
            value_sat: c.value_sat,
            branch: if c.branch == 0 { "receive" } else { "change" },
            reserved: reserved.contains(&c.outpoint),
            in_flight: in_flight.contains(&c.outpoint),
        })
        .collect();
    let mut balances = std::collections::BTreeMap::<(u32, u32), (ScriptBuf, u64)>::new();
    for c in &coins {
        balances
            .entry((c.branch, c.index))
            .or_insert((c.script_pubkey.clone(), 0))
            .1 += c.value_sat;
    }
    let mut ownership = std::collections::HashMap::new();
    if !own.is_empty() {
        for branch in 0..=1 {
            for index in 0..=SCAN_RANGE {
                ownership.insert(
                    change_or_receive_script(&ctx.me, branch, index)?,
                    (branch, index),
                );
            }
        }
    }
    let mut change_total = 0;
    let mut unconfirmed_sat = 0;
    let mut sends = Vec::new();
    for (send, mempool) in own {
        let tx = &send.transaction;
        let amount_sat = tx
            .output
            .first()
            .ok_or("Missing recipient output")?
            .value
            .to_sat();
        let inputs: u64 = send
            .psbt
            .inputs
            .iter()
            .filter_map(|i| i.witness_utxo.as_ref())
            .map(|o| o.value.to_sat())
            .sum();
        let mut own_total = 0;
        for output in &tx.output {
            if let Some(&(branch, index)) = ownership.get(&output.script_pubkey) {
                balances
                    .entry((branch, index))
                    .or_insert((output.script_pubkey.clone(), 0))
                    .1 += output.value.to_sat();
                own_total += output.value.to_sat();
            }
        }
        change_total += own_total;
        unconfirmed_sat += own_total as i64 - inputs as i64;
        sends.push(WalletSendDto {
            txid: tx.compute_txid().to_string(),
            to_address: send.recipient,
            amount_sat,
            fee_sat: send.fee_sat,
            fee_rate: wallet_rate(send.fee_sat, tx.vsize() as u64),
            sent_at: send.sent_at,
            status: if mempool { "mempool" } else { "missing" }.into(),
        });
    }
    sends.sort_by_key(|s| std::cmp::Reverse(s.sent_at));
    let addresses = balances
        .into_iter()
        .map(|(_, (script, balance_sat))| WalletAddressDto {
            address: addr(&script),
            balance_sat,
        })
        .collect();
    Ok(AdminWalletDto {
        unconfirmed_sat,
        addresses,
        sends,
        tip_height,
        total_sat: dtos.iter().map(|c| c.value_sat).sum::<u64>() + change_total,
        available_sat: dtos
            .iter()
            .filter(|c| !c.reserved && !c.in_flight)
            .map(|c| c.value_sat)
            .sum(),
        coins: dtos,
        receive_address: addr(&script),
        receive_index,
        receive_on_device: address_on_device(&ctx.dep, &script),
    })
}

/// Shows receive address `index` on the Ledger and checks that the device derived the same
/// address as this app.
pub async fn show_receive_address(ctx: &PayoutContext, index: u32) -> Result<String, String> {
    let p = policy::admin_wallet_policy(&ctx.me, ctx.dep.network);
    let device = ctx.device.clone();
    let shown = blocking(move || device.show_address(&p, index)).await?;
    let script = change_or_receive_script(&ctx.me, 0, index)?;
    let expected = address_on_device(&ctx.dep, &script).unwrap_or_else(|| {
        bitcoin::Address::from_script(&script, ctx.dep.network)
            .map(|a| a.to_string())
            .unwrap_or_default()
    });
    if shown != expected {
        return Err(format!(
            "The Ledger derived {shown}, not {expected}. Do not send funds to this address."
        ));
    }
    Ok(shown)
}

/// Tells the Admin Wallet panel which coins the pending transactions spend. The panel uses the
/// same wallet; without this it could spend the fee coin of a payout that waits for signatures.
/// Only records that are payouts of this deployment count.
pub(super) fn share_pending_coins(ctx: &PayoutContext, pending: &[CoordinatorPending]) {
    let held = pending.iter().flat_map(|p| {
        let fee_coins = Psbt::from_str(&p.psbt)
            .ok()
            .and_then(|psbt| payout_tx::analyze(&ctx.dep, &psbt).ok())
            .map(|a| a.fee_inputs)
            .unwrap_or_default();
        fee_coins.into_iter().map(|f| (f.outpoint, p.expires_at))
    });
    ctx.reservations.set_held_by_payouts(held, unix_now());
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;

    use bitcoin::hashes::Hash;

    #[test]
    fn the_fee_coins_of_pending_payouts_are_held_for_the_admin_wallet_panel() {
        let (_fx, dep, me, psbt) = setup();
        let ctx = signed_in(&dep, &me, PayoutCoordinator::new("http://127.0.0.1:1", "t"));
        let fee_coin = payout_tx::analyze(&dep, &psbt).unwrap().fee_inputs[0].outpoint;
        let now = unix_now();
        // Not a payout of this deployment: the coordinator cannot hold coins with it.
        let foreign = Psbt::from_unsigned_tx(bitcoin::Transaction {
            version: bitcoin::transaction::Version(3),
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: OutPoint::new(Txid::from_byte_array([9; 32]), 0),
                ..Default::default()
            }],
            output: vec![],
        })
        .unwrap();

        share_pending_coins(
            &ctx,
            &[
                stored_pending(&psbt, now + 3_600),
                stored_pending(&foreign, now + 3_600),
            ],
        );
        assert_eq!(
            ctx.reservations.held_by_payouts(now),
            HashSet::from([fee_coin])
        );
        assert!(ctx.reservations.held_by_payouts(now + 3_600).is_empty());

        // The payout left Pending.
        share_pending_coins(&ctx, &[]);
        assert!(ctx.reservations.held_by_payouts(now).is_empty());
    }

    #[tokio::test]
    async fn a_coin_the_admin_wallet_panel_is_spending_does_not_pay_a_payout_fee() {
        let (_fx, dep, me, _) = setup();
        let ctx = signed_in(&dep, &me, coordinator_answering("[]".into()).await);
        let coin = |n: u8| WalletCoin {
            outpoint: OutPoint::new(Txid::from_byte_array([n; 32]), 0),
            value_sat: 50_000,
            script_pubkey: change_or_receive_script(&me, 0, u32::from(n)).unwrap(),
            branch: 0,
            index: u32::from(n),
            confirmations: 6,
        };
        *ctx.coins.lock().await = Some((
            std::time::Instant::now(),
            ctx.me.index,
            vec![coin(1), coin(2)],
        ));
        ctx.reservations.set_in_flight([coin(1).outpoint]);

        let (usable, _) = plan_inputs(&ctx, &MockNode::default(), false)
            .await
            .unwrap();
        let usable: Vec<OutPoint> = usable.iter().map(|c| c.outpoint).collect();
        assert_eq!(usable, [coin(2).outpoint]);
    }
}
