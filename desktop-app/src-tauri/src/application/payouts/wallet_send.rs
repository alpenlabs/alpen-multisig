use super::{
    address_on_device, amount, change_output, check_app_unchanged, device_sign, fee_input, fees,
    fingerprint_u32, pending_spends, pick_change_index, plan_inputs, policy, text, unix_now,
    wallet_tx, Deserialize, FeeInput, Floors, HashSet, LedgerReviewDto, LedgerStepDto,
    PayoutContext, PayoutNode, Serialize, WalletCoin, WalletSend,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletSendRequest {
    pub address: String,
    pub amount_sat: Option<u64>,
    pub max: bool,
    pub fee_rate: String,
    #[serde(skip)]
    pub expected_txid: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletBumpRequest {
    pub txid: String,
    pub fee_rate: String,
    #[serde(skip)]
    pub expected_txid: Option<String>,
}
/// Shared intent for the review and execute phases. Execution always requires the reviewed id.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WalletIntent {
    Send(WalletSendRequest),
    Bump(WalletBumpRequest),
}

impl WalletIntent {
    fn resolve(
        &self,
        sends: &[(WalletSend, bool)],
    ) -> Result<(WalletSendRequest, Option<WalletSend>), String> {
        match self {
            Self::Send(req) => Ok((req.clone(), None)),
            Self::Bump(req) => {
                let old = original_send(sends, &req.txid)?;
                Ok((bump_request(old, req)?, Some(old.clone())))
            }
        }
    }
}

pub async fn wallet_prepare(
    ctx: &PayoutContext,
    intent: &WalletIntent,
    node: &dyn PayoutNode,
) -> Result<WalletSendReview, String> {
    let _guard = ctx.sends.lock().await;
    let sends = settle_sends(ctx, node).await?;
    check_app_unchanged(ctx).await?;
    let (req, old) = intent.resolve(&sends)?;
    let (built, _) = build_wallet_send(ctx, &req, old.as_ref(), node).await?;
    wallet_review(ctx, &built, &req.address)
}

pub async fn wallet_execute(
    ctx: &PayoutContext,
    intent: &WalletIntent,
    expected_txid: &str,
    node: &dyn PayoutNode,
) -> Result<WalletSent, String> {
    let _guard = ctx.sends.lock().await;
    let sends = settle_sends(ctx, node).await?;
    check_app_unchanged(ctx).await?;
    let (mut req, old) = intent.resolve(&sends)?;
    req.expected_txid = Some(expected_txid.into());
    execute_wallet_send(ctx, &req, old.as_ref(), node).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletSendReview {
    pub txid: String,
    pub to_address: String,
    pub amount_sat: u64,
    pub fee_sat: u64,
    pub fee_rate: String,
    pub change_sat: Option<u64>,
    pub change_address: Option<String>,
    pub review: LedgerReviewDto,
}
#[derive(Debug, Clone, Serialize)]
pub struct WalletSent {
    pub txid: String,
}
/// Remembers the addresses of `coins` as used and returns, per branch, the first index that may
/// still be handed out.
pub(super) fn address_floors(ctx: &PayoutContext, coins: &[WalletCoin]) -> Floors {
    ctx.used_addresses
        .seen(&wallet_key(ctx), coins.iter().map(|c| (c.branch, c.index)))
}
pub(super) fn wallet_key(ctx: &PayoutContext) -> String {
    format!("{}:{}", ctx.dep.network, ctx.me.fingerprint)
}
pub(super) fn wallet_rate(fee: u64, size: u64) -> String {
    format!("{:.1}", fee as f64 / size as f64)
}

pub(super) fn share_wallet_sends(ctx: &PayoutContext) -> Result<(), String> {
    ctx.reservations.set_own_sends(
        ctx.sends
            .get(&wallet_key(ctx))?
            .iter()
            .flat_map(|s| s.transaction.input.iter().map(|i| i.previous_output)),
    );
    Ok(())
}

pub(super) async fn settle_sends(
    ctx: &PayoutContext,
    node: &dyn PayoutNode,
) -> Result<Vec<(WalletSend, bool)>, String> {
    let mut live = Vec::new();
    let mut removed = HashSet::new();
    for send in ctx.sends.get(&wallet_key(ctx))? {
        let mut spent = false;
        for input in &send.transaction.input {
            if !node.unspent(&input.previous_output, false).await? {
                spent = true;
                break;
            }
        }
        if !spent {
            let mempool = node.in_mempool(&send.transaction.compute_txid()).await?;
            live.push((send, mempool));
        } else {
            removed.insert(send.transaction.compute_txid());
        }
    }
    ctx.sends.update(&wallet_key(ctx), |sends| {
        sends.retain(|s| !removed.contains(&s.transaction.compute_txid()))
    })?;
    share_wallet_sends(ctx)?;
    Ok(live)
}

pub(super) async fn build_wallet_send(
    ctx: &PayoutContext,
    req: &WalletSendRequest,
    old: Option<&WalletSend>,
    node: &dyn PayoutNode,
) -> Result<(wallet_tx::BuiltSend, u32), String> {
    let destination = wallet_tx::destination(&req.address, ctx.dep.network)?;
    let rate = fees::parse_fee_rate(&req.fee_rate).map_err(|e| e.to_string())?;
    let (coins, used) = plan_inputs(ctx, node, true).await?;
    let floors = address_floors(ctx, &[]);
    let index = old
        .filter(|s| s.transaction.output.len() > 1)
        .map(|s| s.change_index)
        .map(Ok)
        .unwrap_or_else(|| pick_change_index(&ctx.me, |i| i < floors[1], &used))?;
    let change = change_output(&ctx.dep, &ctx.me, index, 0)?;
    let available = coins
        .iter()
        .map(|c| fee_input(&ctx.dep, &ctx.me, c))
        .collect::<Result<Vec<_>, _>>()?;
    let mut required = Vec::new();
    if let Some(old) = old {
        let pending = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
        let (_, mut reserved, _) = pending_spends(&ctx.dep, &pending);
        reserved.extend(ctx.reservations.in_flight());
        for (txin, input) in old.psbt.unsigned_tx.input.iter().zip(&old.psbt.inputs) {
            if reserved.contains(&txin.previous_output) {
                return Err("Wallet input is reserved.".into());
            }
            let key = input.tap_internal_key.ok_or("Missing input key")?;
            let (_, (fingerprint, path)) = input
                .tap_key_origins
                .get(&key)
                .ok_or("Missing input origin")?;
            required.push(FeeInput {
                outpoint: txin.previous_output,
                txout: input.witness_utxo.clone().ok_or("Missing prevout")?,
                internal_key: key,
                fingerprint: *fingerprint,
                path: path.clone(),
            });
        }
    }
    let amount = if req.max {
        None
    } else {
        Some(req.amount_sat.ok_or("Enter an amount.")?)
    };
    // A Max replacement uses the original inputs only, lowering its recipient amount.
    let extra = if old.is_some() && req.max {
        &[][..]
    } else {
        &available[..]
    };
    let built = wallet_tx::build(
        &required,
        extra,
        destination.script_pubkey(),
        amount,
        rate,
        &change,
    )?;
    if let Some(old) = old {
        if let Some(package) = node
            .mempool_package(&old.transaction.compute_txid())
            .await?
        {
            if package.vsize == 0 {
                return Err("getmempoolentry: invalid descendant size".into());
            }
            wallet_tx::check_descendant_fee(
                package.fee_sat,
                &built,
                node.incremental_relay_tenths().await?,
            )?;
        }

        wallet_tx::check_replacement(
            old.fee_sat,
            old.transaction.vsize() as u64,
            &built,
            node.incremental_relay_tenths().await?,
        )?;
    }
    Ok((built, index))
}
pub(super) fn wallet_review(
    ctx: &PayoutContext,
    built: &wallet_tx::BuiltSend,
    address: &str,
) -> Result<WalletSendReview, String> {
    let tx = &built.psbt.unsigned_tx;
    let txid = tx.compute_txid().to_string();
    let recipient = tx.output.first().ok_or("Missing recipient output")?;
    let amount_sat = recipient.value.to_sat();
    let to =
        address_on_device(&ctx.dep, &recipient.script_pubkey).unwrap_or_else(|| address.into());
    // Every input is the wallet's own, so 2.4.x and 2.5+ show the same three screens (checked on
    // the emulated Nano X with both); "You spend" appears only when a transaction has other inputs.
    let details = vec![
        amount("Amount", amount_sat),
        text("To", to),
        amount("Fees", built.fee_sat),
    ];
    Ok(WalletSendReview {
        txid: txid.clone(),
        to_address: address.into(),
        amount_sat,
        fee_sat: built.fee_sat,
        fee_rate: wallet_rate(built.fee_sat, built.vsize),
        change_sat: tx.output.get(1).map(|o| o.value.to_sat()),
        change_address: tx
            .output
            .get(1)
            .and_then(|o| bitcoin::Address::from_script(&o.script_pubkey, ctx.dep.network).ok())
            .map(|a| a.to_string()),
        review: LedgerReviewDto {
            steps: vec![LedgerStepDto {
                title: "Review transaction → Sign transaction".into(),
                warning: None,
                details,
            }],
            note: ctx.app.screens_note(),
            lock_warning: None,
            txids: vec![txid],
        },
    })
}

pub(super) async fn execute_wallet_send(
    ctx: &PayoutContext,
    req: &WalletSendRequest,
    old: Option<&WalletSend>,
    node: &dyn PayoutNode,
) -> Result<WalletSent, String> {
    let (built, change_index) = build_wallet_send(ctx, req, old, node).await?;
    let txid = built.psbt.unsigned_tx.compute_txid();
    if req.expected_txid.as_deref() != Some(&txid.to_string()) {
        return Err("The transaction changed since you reviewed it. Review it again.".into());
    }
    for input in &built.psbt.unsigned_tx.input {
        // A replacement's inputs are spent by its original mempool transaction; chainstate
        // must still contain them. Newly added inputs must be free even in the mempool.
        let original = old.is_some_and(|s| {
            s.transaction
                .input
                .iter()
                .any(|i| i.previous_output == input.previous_output)
        });
        if !node.unspent(&input.previous_output, !original).await? {
            return Err("Wallet input is already spent.".into());
        }
    }
    let mut held = ctx.reservations.own_sends();
    held.extend(
        built
            .psbt
            .unsigned_tx
            .input
            .iter()
            .map(|i| i.previous_output),
    );
    ctx.reservations.set_own_sends(held);
    // Publish before the final check so governance also sees our selection while the device waits.
    let result = async {
        let in_flight = ctx.reservations.in_flight();
        if built
            .psbt
            .unsigned_tx
            .input
            .iter()
            .any(|i| in_flight.contains(&i.previous_output))
        {
            return Err(
                "Wallet input is now reserved by a governance send. Review it again.".into(),
            );
        }
        let policy = policy::admin_wallet_policy(&ctx.me, ctx.dep.network);
        let signed = device_sign(
            &built.psbt,
            ctx.device.clone(),
            policy,
            None,
            fingerprint_u32(ctx.me.fingerprint),
        )
        .await
        .map_err(|e| {
            if e.contains("Request rejected on device") || e.contains("Deny") || e == "rejected" {
                "rejected".into()
            } else {
                e
            }
        })?;
        let transaction = wallet_tx::finalize(&built.psbt, &signed)?;
        let send = WalletSend {
            transaction: transaction.clone(),
            psbt: built.psbt,
            recipient: req.address.clone(),
            fee_sat: built.fee_sat,
            sent_at: unix_now(),
            max: req.max,
            change_index,
        };
        if let Err(error) = node.broadcast(&transaction).await {
            if !node.in_mempool(&txid).await.unwrap_or(false) {
                return Err(error);
            }
        }
        ctx.sends.update(&wallet_key(ctx), |sends| {
            if let Some(old) = old {
                sends.retain(|s| s.transaction.compute_txid() != old.transaction.compute_txid());
            }
            sends.push(send);
        })?;
        *ctx.coins.lock().await = None;
        Ok(WalletSent {
            txid: txid.to_string(),
        })
    }
    .await;
    share_wallet_sends(ctx)?;
    result
}

pub(super) fn bump_request(
    old: &WalletSend,
    req: &WalletBumpRequest,
) -> Result<WalletSendRequest, String> {
    Ok(WalletSendRequest {
        address: old.recipient.clone(),
        amount_sat: Some(
            old.transaction
                .output
                .first()
                .ok_or("Missing recipient output")?
                .value
                .to_sat(),
        ),
        max: old.max,
        fee_rate: req.fee_rate.clone(),
        expected_txid: req.expected_txid.clone(),
    })
}

fn original_send<'a>(
    sends: &'a [(WalletSend, bool)],
    txid: &str,
) -> Result<&'a WalletSend, String> {
    sends
        .iter()
        .find(|(s, _)| s.transaction.compute_txid().to_string() == txid)
        .map(|(s, _)| s)
        .ok_or_else(|| "Unknown unconfirmed send".into())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;

    #[tokio::test]
    async fn governance_race_stops_before_device_and_releases_inputs() {
        let (ctx, mut node, req, device) = send_fixture(false).await;
        node.reserve_on_unspent = Some(ctx.reservations.clone());
        let error = execute_wallet_send(&ctx, &req, None, &node)
            .await
            .err()
            .unwrap();
        assert!(error.contains("governance"), "{error}");
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(ctx.reservations.own_sends().is_empty());
    }

    #[tokio::test]
    async fn rejected_replacement_keeps_original_and_timeout_acceptance_reconciles() {
        let (ctx, mut node, req, _) = send_fixture(false).await;
        let original = wallet_execute(
            &ctx,
            &WalletIntent::Send(req.clone()),
            req.expected_txid.as_deref().unwrap_or(""),
            &node,
        )
        .await
        .unwrap();
        node.mempool.insert(original.txid.parse().unwrap());
        let before = admin_wallet(&ctx, &node).await.unwrap();
        let mut bump = WalletBumpRequest {
            txid: original.txid.clone(),
            fee_rate: "5".into(),
            expected_txid: None,
        };
        bump.expected_txid = Some(
            wallet_prepare(&ctx, &WalletIntent::Bump(bump.clone()), &node)
                .await
                .unwrap()
                .txid,
        );
        node.reject = true;
        assert_eq!(
            wallet_execute(
                &ctx,
                &WalletIntent::Bump(bump.clone()),
                bump.expected_txid.as_deref().unwrap_or(""),
                &node
            )
            .await
            .err()
            .unwrap(),
            "node refused replacement"
        );
        let after = admin_wallet(&ctx, &node).await.unwrap();
        assert_eq!(after.sends[0].txid, original.txid);
        assert_eq!(
            (after.total_sat, after.unconfirmed_sat),
            (before.total_sat, before.unconfirmed_sat)
        );
        node.accepted_on_error = true;
        let replaced = wallet_execute(
            &ctx,
            &WalletIntent::Bump(bump.clone()),
            bump.expected_txid.as_deref().unwrap_or(""),
            &node,
        )
        .await
        .unwrap();
        assert_eq!(
            ctx.sends.get(&wallet_key(&ctx)).unwrap()[0]
                .transaction
                .compute_txid()
                .to_string(),
            replaced.txid
        );
    }

    #[tokio::test]
    async fn self_send_normal_and_max_only_lose_fee_before_and_after_confirmation() {
        for max in [false, true] {
            let (ctx, mut node, req, _) = send_fixture(max).await;
            wallet_execute(
                &ctx,
                &WalletIntent::Send(req.clone()),
                req.expected_txid.as_deref().unwrap_or(""),
                &node,
            )
            .await
            .unwrap();
            let send = ctx.sends.get(&wallet_key(&ctx)).unwrap().remove(0);
            let before = admin_wallet(&ctx, &node).await.unwrap();
            assert_eq!(before.total_sat, 100_000 - send.fee_sat);
            assert_eq!(before.unconfirmed_sat, -(send.fee_sat as i64));
            assert_eq!(before.available_sat, 0);
            assert_eq!(
                before.addresses.iter().map(|a| a.balance_sat).sum::<u64>(),
                before.total_sat
            );
            assert!(before.addresses.iter().any(|a| a.address == req.address));
            node.spent
                .extend(send.transaction.input.iter().map(|i| i.previous_output));
            node.scanned.clear();
            for (vout, output) in send.transaction.output.iter().enumerate() {
                let point = OutPoint::new(send.transaction.compute_txid(), vout as u32);
                node.outputs.insert(point, output.clone());
                let (branch, index) = if vout == 0 {
                    (0, 1)
                } else {
                    (1, send.change_index)
                };
                node.scanned
                    .push(crate::infrastructure::payout_node::ScannedCoin {
                        outpoint: point,
                        value_sat: output.value.to_sat(),
                        script_pubkey: output.script_pubkey.clone(),
                        desc: format!("tr([00000000/86h/1h/73h/{branch}/{index}]key)"),
                        height: 160,
                    });
            }
            let after = admin_wallet(&ctx, &node).await.unwrap();
            assert_eq!(after.total_sat, before.total_sat);
            assert_eq!(after.available_sat, after.total_sat);
            assert_eq!(after.unconfirmed_sat, 0);
            assert!(ctx.reservations.own_sends().is_empty());
        }
    }

    #[tokio::test]
    async fn descendant_fee_stops_bump_at_review() {
        let (ctx, mut node, req, device) = send_fixture(false).await;
        let sent = wallet_execute(
            &ctx,
            &WalletIntent::Send(req.clone()),
            req.expected_txid.as_deref().unwrap_or(""),
            &node,
        )
        .await
        .unwrap();
        node.descendants = Some((10_308, 300));
        let bump = WalletBumpRequest {
            txid: sent.txid,
            fee_rate: "5".into(),
            expected_txid: None,
        };
        let error = wallet_prepare(&ctx, &WalletIntent::Bump(bump.clone()), &node)
            .await
            .unwrap_err();
        assert!(error.contains("at least 68.0 sat/vB"), "{error}");
        assert_eq!(device.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn damaged_store_blocks_every_wallet_command_without_writing() {
        let (mut ctx, node, req, _) = send_fixture(false).await;
        let dir = std::env::temp_dir().join(format!("damaged-wallet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payout-wallet-sends.json");
        let (built, index) = build_wallet_send(&ctx, &req, None, &node).await.unwrap();
        let mut bad = WalletSend {
            transaction: built.psbt.unsigned_tx.clone(),
            psbt: built.psbt,
            recipient: req.address.clone(),
            fee_sat: built.fee_sat + 1,
            sent_at: 1,
            max: false,
            change_index: index,
        };
        let wrong_fee =
            serde_json::to_vec(&HashMap::from([(wallet_key(&ctx), vec![bad.clone()])])).unwrap();
        bad.transaction.output.clear();
        let no_outputs =
            serde_json::to_vec(&HashMap::from([(wallet_key(&ctx), vec![bad])])).unwrap();
        ctx.sends = WalletSendStore::new(Some(dir.clone()));
        for bytes in [b"{truncated".to_vec(), wrong_fee, no_outputs] {
            std::fs::write(&path, &bytes).unwrap();
            let bump = WalletBumpRequest {
                txid: "unknown".into(),
                fee_rate: "5".into(),
                expected_txid: None,
            };
            let errors = [
                admin_wallet(&ctx, &node).await.err(),
                wallet_prepare(&ctx, &WalletIntent::Send(req.clone()), &node)
                    .await
                    .err(),
                wallet_execute(
                    &ctx,
                    &WalletIntent::Send(req.clone()),
                    req.expected_txid.as_deref().unwrap_or(""),
                    &node,
                )
                .await
                .err(),
                wallet_prepare(&ctx, &WalletIntent::Bump(bump.clone()), &node)
                    .await
                    .err(),
                wallet_execute(
                    &ctx,
                    &WalletIntent::Bump(bump.clone()),
                    bump.expected_txid.as_deref().unwrap_or(""),
                    &node,
                )
                .await
                .err(),
            ];
            for error in errors {
                let error = error.unwrap();
                assert!(
                    error.contains("damaged and was left untouched")
                        && error.contains(path.to_str().unwrap()),
                    "{error}"
                );
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn own_sends_settle_and_do_not_reuse_change_or_reserved_inputs() {
        let (fx, dep, me, payout) = setup();
        let mut ctx = signed_in(&dep, &me, coordinator_answering("[]".into()).await);
        let coin = fee_coin(&fx, 0, 0, 50_000);
        let built = wallet_tx::build(
            &[],
            std::slice::from_ref(&coin),
            change(&fx, 0).script_pubkey(),
            Some(10_000),
            20,
            &change(&fx, 0),
        )
        .unwrap();
        let mut node = node_for(&built.psbt);
        node.scanned
            .push(crate::infrastructure::payout_node::ScannedCoin {
                outpoint: coin.outpoint,
                value_sat: 50_000,
                script_pubkey: coin.txout.script_pubkey.clone(),
                desc: "tr([00000000/86h/1h/73h/0/0]key)".into(),
                height: 100,
            });
        let req = WalletSendRequest {
            address: bitcoin::Address::from_script(&change(&fx, 0).script_pubkey(), dep.network)
                .unwrap()
                .to_string(),
            amount_sat: Some(10_000),
            max: false,
            fee_rate: "2".into(),
            expected_txid: None,
        };
        ctx.reservations.set_in_flight([coin.outpoint]);
        assert_eq!(
            build_wallet_send(&ctx, &req, None, &node)
                .await
                .err()
                .unwrap(),
            "Insufficient funds"
        );
        ctx.reservations.set_in_flight([]);
        assert!(build_wallet_send(&ctx, &req, None, &node).await.is_ok());
        let mut record = serde_json::to_value(stored_pending(&payout, unix_now() + 3600)).unwrap();
        record["connector_count"] = serde_json::json!(0);
        record["raw_tx"] = serde_json::json!("");
        record["broadcast_at"] = serde_json::json!(0);
        ctx.coordinator =
            Arc::new(coordinator_answering(serde_json::json!([record]).to_string()).await);
        assert_eq!(
            build_wallet_send(&ctx, &req, None, &node)
                .await
                .err()
                .unwrap(),
            "Insufficient funds"
        );
        ctx.coordinator = Arc::new(coordinator_answering("[]".into()).await);
        let send = WalletSend {
            transaction: built.psbt.unsigned_tx.clone(),
            psbt: built.psbt,
            recipient: req.address,
            fee_sat: built.fee_sat,
            sent_at: 1,
            max: false,
            change_index: 0,
        };
        ctx.sends
            .save(&wallet_key(&ctx), vec![send.clone()])
            .unwrap();
        assert!(!settle_sends(&ctx, &node).await.unwrap()[0].1);
        node.mempool.insert(send.transaction.compute_txid());
        node.mempool_spent.insert(coin.outpoint);
        assert!(!node.unspent(&coin.outpoint, true).await.unwrap());
        assert!(node.unspent(&coin.outpoint, false).await.unwrap());
        assert!(ctx.reservations.own_sends().contains(&coin.outpoint));
        assert!(settle_sends(&ctx, &node).await.unwrap()[0].1);
        let used = HashSet::from([change_script(&me, 0).unwrap()]);
        assert_eq!(pick_change_index(&me, |_| false, &used).unwrap(), 1);
        node.spent.insert(coin.outpoint);
        assert!(settle_sends(&ctx, &node).await.unwrap().is_empty());
        assert!(ctx.sends.get(&wallet_key(&ctx)).unwrap().is_empty());
    }
}
