use super::{
    admin_label, check_app_unchanged, ledger_review_for, payout_tx, policy, sign_connectors,
    sign_fee_inputs, AdminSigner, Arc, BTreeSet, ChildNumber, CoordinatorOutcome, CoordinatorPast,
    CoordinatorPending, Deployment, FromStr, HashSet, LedgerReviewDto, PayoutContext,
    PayoutCoordinator, PayoutNode, PlanClaim, PolicyStore, Psbt, ScriptBuf, Secp256k1, Serialize,
    Txid, XOnlyPublicKey, SCAN_RANGE,
};
use super::{ClaimInput, PayoutDevice};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputDto {
    pub outpoint: String,
    /// Also an input of another pending transaction.
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignerDto {
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDto {
    pub txid: String,
    pub psbt: String,
    pub created_by: String,
    pub expires_at: i64,
    pub connector_count: usize,
    pub inputs: Vec<PendingInputDto>,
    pub has_duplicates: bool,
    pub signatures_received: usize,
    pub signatures_needed: usize,
    pub signers: Vec<SignerDto>,
    pub signed_by_me: bool,
    pub complete: bool,
    pub broadcast_error: Option<String>,
    pub fee_sat: u64,
    pub vsize: u64,
    /// The Ledger's name for the multisig wallet of each covenant this transaction spends
    /// (`Payout <id>`), as shown on the device at registration and signing.
    pub policy_names: Vec<String>,
    /// The change output, read from the PSBT here (not from the coordinator). The Ledger lists it
    /// as a "To" address when signing the claim inputs; admins compare it with this.
    pub change_address: Option<String>,
    /// On regtest only: the same change address as the Ledger shows it. The Bitcoin app has no
    /// regtest encoding and displays testnet (`tb1…`), so the app's `bcrt1…` would not match.
    pub change_address_on_device: Option<String>,
    pub change_sat: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastDto {
    pub txid: String,
    pub raw_tx: String,
    /// `confirmed` | `unconfirmed`.
    pub status: String,
    pub block_timestamp: Option<u64>,
    pub block_height: Option<u64>,
    pub connector_count: usize,
    pub broadcast_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayoutsOverviewDto {
    pub pending: Vec<PendingDto>,
    pub past: Vec<PastDto>,
    /// Set when the node could not be asked; statuses may be stale.
    pub node_warning: Option<String>,
}

pub(super) fn label_of_key(dep: &Deployment, xonly: &str) -> String {
    XOnlyPublicKey::from_str(xonly)
        .ok()
        .and_then(|k| dep.admin_index(&k))
        .map(|i| admin_label(dep, i))
        .unwrap_or_else(|| xonly.to_string())
}

/// On regtest only: `script` as the Ledger shows it. The Bitcoin app has no regtest encoding and
/// displays testnet (`tb1…`), so the app's `bcrt1…` would not match.
pub(super) fn address_on_device(dep: &Deployment, script: &ScriptBuf) -> Option<String> {
    (dep.network == bitcoin::Network::Regtest)
        .then(|| bitcoin::Address::from_script(script, bitcoin::Network::Testnet).ok())
        .flatten()
        .map(|a| a.to_string())
}

/// A pending transaction as the screen shows it. What matters for signing (the inputs, who has
/// signed, quorum, fee, size) is read from the PSBT here, where every signature is verified; the
/// coordinator's own summary of these is not trusted. An error means the coordinator's record
/// is not a payout transaction of this deployment.
pub(super) fn pending_dto(
    dep: &Deployment,
    me: Option<usize>,
    p: &CoordinatorPending,
) -> Result<PendingDto, String> {
    let psbt = Psbt::from_str(&p.psbt).map_err(|e| e.to_string())?;
    let a = payout_tx::analyze(dep, &psbt).map_err(|e| e.to_string())?;
    if a.txid.to_string() != p.txid {
        return Err(format!("it is filed as {} but is {}", p.txid, a.txid));
    }
    let dups: HashSet<&String> = p.duplicate_outpoints.iter().collect();
    let change = psbt.unsigned_tx.output.first();
    let policy_names = a
        .connector_inputs
        .iter()
        .map(|c| c.covenant_index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|i| policy::admin_policy_name(&dep.covenants[i]))
        .collect();
    let outpoints: Vec<String> = a.outpoints().iter().map(|o| o.to_string()).collect();
    Ok(PendingDto {
        policy_names,
        change_address: change
            .and_then(|o| bitcoin::Address::from_script(&o.script_pubkey, dep.network).ok())
            .map(|a| a.to_string()),
        change_address_on_device: change.and_then(|o| address_on_device(dep, &o.script_pubkey)),
        change_sat: a.change_sat,
        txid: p.txid.clone(),
        psbt: p.psbt.clone(),
        created_by: label_of_key(dep, &p.created_by),
        expires_at: p.expires_at,
        connector_count: outpoints.len(),
        has_duplicates: outpoints.iter().any(|o| dups.contains(o)),
        inputs: outpoints
            .into_iter()
            .map(|o| PendingInputDto {
                duplicate: dups.contains(&o),
                outpoint: o,
            })
            .collect(),
        signatures_received: a.signatures_received,
        signatures_needed: a.signatures_needed,
        signers: a
            .signers
            .iter()
            .map(|&i| SignerDto {
                label: admin_label(dep, i),
            })
            .collect(),
        signed_by_me: me.is_some_and(|m| a.signed_by(m)),
        complete: a.complete,
        broadcast_error: p.broadcast_error.clone(),
        fee_sat: a.fee_sat,
        vsize: a.vsize,
    })
}

/// What the admin's node says about one pending transaction.
pub(super) enum PendingCheck {
    Live,
    /// The node knows the transaction itself.
    Broadcast,
    /// An input was spent in a block by something else.
    InputSpent,
}

pub(super) async fn check_pending(
    node: &dyn PayoutNode,
    p: &CoordinatorPending,
) -> Result<PendingCheck, String> {
    let psbt = Psbt::from_str(&p.psbt).map_err(|e| e.to_string())?;
    let txid = psbt.unsigned_tx.compute_txid();
    if node.tx(&txid).await?.is_some() {
        return Ok(PendingCheck::Broadcast);
    }
    for i in &psbt.unsigned_tx.input {
        if !node.unspent(&i.previous_output, false).await? {
            return Ok(PendingCheck::InputSpent);
        }
    }
    Ok(PendingCheck::Live)
}

pub(super) async fn past_dto(
    node: &dyn PayoutNode,
    p: &CoordinatorPast,
) -> Result<PastDto, String> {
    let txid = Txid::from_str(&p.txid).map_err(|e| e.to_string())?;
    let found = node.tx(&txid).await?;
    Ok(past_record_dto(p, found))
}

fn past_record_dto(
    p: &CoordinatorPast,
    found: Option<crate::infrastructure::payout_node::NodeTx>,
) -> PastDto {
    let (status, block_time, block_height) = match found {
        Some(t) if t.confirmations > 0 => ("confirmed", t.block_time, t.block_height),
        _ => ("unconfirmed", None, None),
    };
    PastDto {
        txid: p.txid.clone(),
        raw_tx: p.raw_tx.clone(),
        status: status.to_string(),
        block_timestamp: block_time.map(|t| t * 1000),
        block_height,
        connector_count: p.connector_count,
        broadcast_at: p.broadcast_at,
    }
}

/// Oldest Bitcoin Core that relays version 3 (TRUC) transactions, which every payout is.
pub const MIN_CORE_VERSION: u64 = 280_000;

/// A warning when the node is too old to relay payouts.
pub(super) fn core_version_warning(version: u64) -> Option<String> {
    (version < MIN_CORE_VERSION).then(|| {
        format!(
            "Bitcoin Core {}.{} is older than 28.0: it does not relay version 3 (TRUC) \
             transactions, so it would reject every payout at broadcast. Update the node.",
            version / 10_000,
            version / 100 % 100
        )
    })
}

/// The Pending and Past tabs. Pending transactions the node shows as broadcast, or with an input
/// spent elsewhere, are handed to the coordinator to check with its own node and drop.
pub async fn overview(
    dep: &Deployment,
    me: Option<usize>,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<PayoutsOverviewDto, String> {
    let stored = coordinator.pending().await.map_err(|e| e.to_string())?;
    overview_of(dep, me, stored, coordinator, node).await
}

/// [`overview`] for the signed-in admin. It also tells the Admin Wallet panel which coins the
/// pending transactions spend.
pub async fn overview_for(
    ctx: &PayoutContext,
    node: &dyn PayoutNode,
) -> Result<PayoutsOverviewDto, String> {
    let stored = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
    overview_of(&ctx.dep, Some(ctx.me.index), stored, &ctx.coordinator, node).await
}

pub(super) async fn overview_of(
    dep: &Deployment,
    me: Option<usize>,
    stored: Vec<CoordinatorPending>,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<PayoutsOverviewDto, String> {
    let mut node_warning = node.version().await.ok().and_then(core_version_warning);
    let mut pending = Vec::new();
    for p in stored {
        // A record that is not a payout of this deployment is not shown: nothing on it can be
        // trusted, and it cannot be signed.
        let dto = match pending_dto(dep, me, &p) {
            Ok(dto) => dto,
            Err(e) => {
                node_warning = Some(format!(
                    "The coordinator holds a pending transaction this app cannot verify ({e}); it is not shown."
                ));
                continue;
            }
        };
        match check_pending(node, &p).await {
            Ok(PendingCheck::Live) => pending.push(dto),
            Ok(PendingCheck::Broadcast | PendingCheck::InputSpent) => {
                match coordinator.refresh(&p.txid).await {
                    Ok(CoordinatorOutcome::Unchanged) => pending.push(dto),
                    // Your node sees this payout broadcast or an input of it spent, but the
                    // coordinator could not confirm that with its own node: say so instead of
                    // showing it as an ordinary pending payout.
                    Err(e) => {
                        node_warning = Some(format!(
                            "The coordinator could not check payout {} against its Bitcoin Core node: {e}",
                            p.txid
                        ));
                        pending.push(dto);
                    }
                    Ok(_) => {}
                }
            }
            Err(e) => {
                node_warning = Some(format!("Bitcoin Core could not be asked: {e}"));
                pending.push(dto);
            }
        }
    }
    let mut past = Vec::new();
    for p in coordinator.past().await.map_err(|e| e.to_string())? {
        match past_dto(node, &p).await {
            Ok(d) => past.push(d),
            Err(e) => {
                node_warning = Some(format!("Bitcoin Core could not be asked: {e}"));
                past.push(past_record_dto(&p, None));
            }
        }
    }
    Ok(PayoutsOverviewDto {
        pending,
        past,
        node_warning,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastDto {
    pub txid: String,
    pub broadcast: bool,
    pub error: Option<String>,
}

/// Finalizes and broadcasts a complete transaction, and tells the coordinator how it went.
pub(super) async fn broadcast_pending(
    dep: &Deployment,
    psbt: &Psbt,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<BroadcastDto, String> {
    let tx = payout_tx::finalize(dep, psbt).map_err(|e| e.to_string())?;
    let txid = tx.compute_txid().to_string();
    // Once this node has the transaction it is broadcast, whether or not the coordinator can be
    // told right now (it may be unreachable, or its own node may not have seen the transaction
    // yet): the next refresh of the Pending list reconciles it.
    let error = node
        .broadcast(&tx)
        .await
        .err()
        .filter(|e| !already_known(e));
    let _ = coordinator.report_broadcast(&txid, error.as_deref()).await;
    Ok(BroadcastDto {
        txid,
        broadcast: error.is_none(),
        error,
    })
}

/// After any change: broadcast as soon as the transaction is complete.
pub(super) async fn broadcast_if_complete(
    dep: &Deployment,
    pending: &CoordinatorPending,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<Option<BroadcastDto>, String> {
    // Whether it is complete is read from its signatures here, not from the coordinator's flag.
    let psbt = Psbt::from_str(&pending.psbt).map_err(|e| e.to_string())?;
    if !payout_tx::analyze(dep, &psbt)
        .map_err(|e| e.to_string())?
        .complete
    {
        return Ok(None);
    }
    broadcast_pending(dep, &psbt, coordinator, node)
        .await
        .map(Some)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResultDto {
    /// Rust batch callers use the submitted id; the UI only consumes the broadcast outcome.
    #[serde(skip)]
    pub txid: String,
    pub broadcast: Option<BroadcastDto>,
}

/// Signs each transaction on the device (claim inputs, then fee inputs), stores it on the
/// coordinator and broadcasts it if that already reaches quorum. A failure after earlier
/// transactions were submitted says so: those stay in Pending.
pub(super) async fn sign_and_submit(
    ctx: &PayoutContext,
    psbts: Vec<Psbt>,
    store: &PolicyStore,
    node: &dyn PayoutNode,
) -> Result<Vec<SubmitResultDto>, String> {
    let total = psbts.len();
    let mut out: Vec<SubmitResultDto> = Vec::new();
    for mut psbt in psbts {
        let submitted = async {
            sign_connectors(&ctx.dep, &ctx.me, &mut psbt, ctx.device.clone(), store).await?;
            sign_fee_inputs(&ctx.dep, &ctx.me, &mut psbt, ctx.device.clone()).await?;
            submit_and_broadcast(&ctx.dep, &psbt, &ctx.coordinator, node, true).await
        };
        let pending = match submitted.await {
            Ok(p) => p,
            Err(e) if out.is_empty() => return Err(e),
            Err(e) => {
                return Err(format!(
                    "Transaction {this} of {total} failed: {e}\n{before} of {total} were signed and \
                     submitted before it and are in Pending. If the coordinator could not be reached, \
                     transaction {this} may be there too. The ones after it were not created.",
                    this = out.len() + 1,
                    before = out.len()
                ))
            }
        };
        out.push(pending);
    }
    Ok(out)
}

/// Highest fee rate the fee field accepts, in sat/vB; no payout may pay more.
pub(super) const MAX_FEE_RATE_SAT_VB: u64 = payout_admin_core::fees::MAX_FEE_RATE_TENTHS / 10;

/// A pending transaction that came from the coordinator, checked before this admin reviews or
/// signs it.
pub struct CheckedPending {
    pub psbt: Psbt,
    /// The Payout Administrator whose Admin Wallet receives the change.
    pub change_owner: usize,
}

/// The roster admin whose Admin Wallet (first [`SCAN_RANGE`] change and receive addresses)
/// `script` belongs to.
pub(super) fn admin_wallet_owner(dep: &Deployment, script: &ScriptBuf) -> Option<usize> {
    let secp = Secp256k1::verification_only();
    for signer in dep.signers.as_deref()? {
        for branch in [1, 0] {
            let branch_key = signer
                .account_xpub
                .derive_pub(&secp, &[ChildNumber::from_normal_idx(branch).ok()?])
                .ok()?;
            for index in 0..=SCAN_RANGE {
                let key = branch_key
                    .derive_pub(&secp, &[ChildNumber::from_normal_idx(index).ok()?])
                    .ok()?;
                let candidate =
                    ScriptBuf::new_p2tr(&secp, key.public_key.x_only_public_key().0, None);
                if candidate == *script {
                    return Some(signer.index);
                }
            }
        }
    }
    None
}

/// Checks a pending transaction the coordinator returned for `txid` before the device is asked
/// to sign it. The coordinator and the admin who created the transaction are not trusted here:
///
/// - it is the transaction that was asked for (its own id is `txid`);
/// - it is a payout of this deployment (shape, claim inputs of trusted covenants, size), with
///   signing data rewritten from the deployment and invalid signatures dropped;
/// - every input exists on this admin's Bitcoin Core node with the amount and script the
///   transaction states, and is unspent, so the fee the device shows is the real one;
/// - the fee rate is within what the fee field allows;
/// - the single output pays an Admin Wallet address of one of the Payout Administrators.
pub(super) async fn check_pending_for_signing(
    dep: &Deployment,
    txid: &str,
    psbt_b64: &str,
    node: &dyn PayoutNode,
) -> Result<CheckedPending, String> {
    let mut psbt = Psbt::from_str(psbt_b64).map_err(|e| e.to_string())?;
    let a = payout_tx::normalize(dep, &mut psbt).map_err(|e| e.to_string())?;
    if a.txid.to_string() != txid.trim() {
        return Err(format!(
            "The coordinator returned transaction {} instead of {txid}. Nothing was signed.",
            a.txid
        ));
    }
    for (txin, input) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs) {
        let o = txin.previous_output;
        let stated = input
            .witness_utxo
            .as_ref()
            .ok_or_else(|| format!("Input {o} does not state what it spends."))?;
        let known = node.tx(&o.txid).await?.ok_or_else(|| {
            format!("Bitcoin Core does not know the transaction of input {o}. Nothing was signed.")
        })?;
        if known.tx.output.get(o.vout as usize) != Some(stated) {
            return Err(format!(
                "Input {o} is not what this transaction says it is: the amount or script differs on \
                 your Bitcoin Core node. Nothing was signed."
            ));
        }
        if !node.unspent(&o, true).await? {
            return Err(format!("Input {o} is already spent. Nothing was signed."));
        }
    }
    if a.fee_sat > a.vsize * MAX_FEE_RATE_SAT_VB {
        return Err(format!(
            "This transaction pays a fee of {} sat for {} vB, above the highest fee rate this app \
             allows ({MAX_FEE_RATE_SAT_VB} sat/vB). Nothing was signed.",
            a.fee_sat, a.vsize
        ));
    }
    let change = psbt.unsigned_tx.output.first().ok_or("no change output")?;
    let change_owner = admin_wallet_owner(dep, &change.script_pubkey).ok_or_else(|| {
        format!(
            "The output of this transaction ({}) is not an Admin Wallet address of any Payout \
             Administrator (the first {SCAN_RANGE} addresses of each were checked). Do not sign it.",
            bitcoin::Address::from_script(&change.script_pubkey, dep.network)
                .map(|a| a.to_string())
                .unwrap_or_default()
        )
    })?;
    Ok(CheckedPending { psbt, change_owner })
}

/// Sign on a pending transaction: the device adds this admin's signatures to every claim input.
/// The transaction is checked first, see [`check_pending_for_signing`].
pub async fn sign_pending(
    dep: &Deployment,
    me: &AdminSigner,
    txid: &str,
    device: Arc<dyn PayoutDevice>,
    store: &PolicyStore,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<SubmitResultDto, String> {
    let mut psbt = fetch_checked_pending(dep, txid, coordinator, node)
        .await?
        .psbt;
    sign_connectors(dep, me, &mut psbt, device, store).await?;
    submit_and_broadcast(dep, &psbt, coordinator, node, false).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteResultDto {
    pub added: usize,
    pub already_present: usize,
    /// The warning listing every invalid signature, when there was one.
    pub message: Option<String>,
    pub broadcast: Option<BroadcastDto>,
}

pub async fn paste_signatures(
    dep: &Deployment,
    txid: &str,
    text: &str,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<PasteResultDto, String> {
    // What was added and how many admins have signed is counted here from verified signatures,
    // before and after; the coordinator's own numbers are not what the screen reports.
    let verified = |psbt_b64: &str| -> Result<(usize, payout_tx::Analysis), String> {
        let psbt = Psbt::from_str(psbt_b64).map_err(|e| e.to_string())?;
        let a = payout_tx::analyze(dep, &psbt).map_err(|e| e.to_string())?;
        if a.txid.to_string() != txid.trim() {
            return Err(format!(
                "The coordinator returned transaction {} instead of {txid}.",
                a.txid
            ));
        }
        let signatures = a.connector_inputs.iter().map(|c| c.signed_by.len()).sum();
        Ok((signatures, a))
    };
    let before = coordinator
        .get_pending(txid)
        .await
        .map_err(|e| e.to_string())?;
    let (had, _) = verified(&before.psbt)?;
    let r = coordinator
        .paste(txid, text)
        .await
        .map_err(|e| e.to_string())?;
    let (has, _) = verified(&r.pending.psbt)?;
    // Also when nothing was added: a complete transaction whose broadcast never happened (a lost
    // response, a closed app) is sent now.
    let broadcast = finish_submission(dep, r.pending, coordinator, node, false)
        .await?
        .broadcast;
    Ok(PasteResultDto {
        added: has.saturating_sub(had),
        already_present: r.already_present,
        message: r.message,
        broadcast,
    })
}

/// Import: a PSBT (base64) of a payout transaction, new or carrying more signatures.
pub async fn import(
    dep: &Deployment,
    text: &str,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<SubmitResultDto, String> {
    let mut psbt = Psbt::from_str(text.trim()).map_err(|_| {
        "Paste a payout transaction exported from this app (PSBT, base64).".to_string()
    })?;
    payout_tx::normalize(dep, &mut psbt).map_err(|e| e.to_string())?;
    submit_and_broadcast(dep, &psbt, coordinator, node, false).await
}

/// Copy all signatures: one line per admin signature.
pub fn signatures_text(dep: &Deployment, psbt_b64: &str) -> Result<String, String> {
    let psbt = Psbt::from_str(psbt_b64).map_err(|e| e.to_string())?;
    let lines = payout_tx::export_signatures(dep, &psbt).map_err(|e| e.to_string())?;
    Ok(payout_tx::signatures_text(&lines))
}

pub async fn retry_broadcast(
    dep: &Deployment,
    txid: &str,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<BroadcastDto, String> {
    let p = coordinator
        .get_pending(txid)
        .await
        .map_err(|e| e.to_string())?;
    let psbt = Psbt::from_str(&p.psbt).map_err(|e| e.to_string())?;
    if psbt.unsigned_tx.compute_txid().to_string() != txid.trim() {
        return Err(format!(
            "The coordinator returned another transaction instead of {txid}; it was not sent."
        ));
    }
    broadcast_pending(dep, &psbt, coordinator, node).await
}

/// Rebroadcast from the Past tab (still unconfirmed). The coordinator cannot have this node relay
/// something else: the record's PSBT must be a payout of this deployment, and the raw transaction
/// sent must be that very transaction with the id `txid` (an id commits to every input and
/// output). The PSBT itself may lack signatures the raw transaction has, when the payout was
/// completed outside the coordinator.
pub async fn rebroadcast(
    dep: &Deployment,
    txid: &str,
    psbt_b64: &str,
    raw_tx: &str,
    node: &dyn PayoutNode,
) -> Result<String, String> {
    let psbt = Psbt::from_str(psbt_b64).map_err(|e| e.to_string())?;
    let a = payout_tx::analyze(dep, &psbt).map_err(|e| e.to_string())?;
    let tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize_hex(raw_tx.trim())
        .map_err(|e| format!("invalid transaction: {e}"))?;
    if a.txid.to_string() != txid.trim() || tx.compute_txid() != a.txid {
        return Err(format!(
            "The coordinator's record is not the transaction {txid}; it was not sent."
        ));
    }
    match node.broadcast(&tx).await {
        Ok(t) => Ok(t.to_string()),
        Err(e) if already_known(&e) => Ok(tx.compute_txid().to_string()),
        Err(e) => Err(e),
    }
}

async fn submit_and_broadcast(
    dep: &Deployment,
    psbt: &Psbt,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
    retain_broadcast_error: bool,
) -> Result<SubmitResultDto, String> {
    let pending = coordinator
        .submit(&psbt.to_string())
        .await
        .map_err(|e| e.to_string())?;
    finish_submission(dep, pending, coordinator, node, retain_broadcast_error).await
}

async fn finish_submission(
    dep: &Deployment,
    pending: CoordinatorPending,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
    retain_broadcast_error: bool,
) -> Result<SubmitResultDto, String> {
    // Batch submissions remain in Pending even if broadcasting fails; keep processing the batch.
    let broadcast = match broadcast_if_complete(dep, &pending, coordinator, node).await {
        Ok(b) => b,
        Err(e) if retain_broadcast_error => Some(BroadcastDto {
            txid: pending.txid.clone(),
            broadcast: false,
            error: Some(e),
        }),
        Err(e) => return Err(e),
    };
    Ok(SubmitResultDto {
        txid: pending.txid,
        broadcast,
    })
}

fn already_known(e: &str) -> bool {
    e.contains("already in") || e.contains("txn-already-known")
}

pub(super) async fn fetch_checked_pending(
    dep: &Deployment,
    txid: &str,
    coordinator: &PayoutCoordinator,
    node: &dyn PayoutNode,
) -> Result<CheckedPending, String> {
    let current = coordinator
        .get_pending(txid)
        .await
        .map_err(|e| e.to_string())?;
    check_pending_for_signing(dep, txid, &current.psbt, node).await
}

/// Re-checks, right before signing, that each claim input is the covenant's connector and still
/// unspent on the admin's node.
pub async fn check_claims_on_node(
    dep: &Deployment,
    claims: &[ClaimInput],
    node: &dyn PayoutNode,
) -> Result<(), String> {
    for c in claims {
        let pc = PlanClaim::try_from(c)?;
        let o = pc.connector.outpoint();
        let cov = dep
            .covenants
            .get(pc.connector.covenant_index)
            .ok_or_else(|| format!("{o}: unknown covenant"))?;
        let t = node
            .tx(&o.txid)
            .await?
            .ok_or_else(|| format!("Bitcoin Core does not know the claim {}", o.txid))?;
        let out =
            t.tx.output
                .get(o.vout as usize)
                .ok_or_else(|| format!("{o} does not exist"))?;
        if out.script_pubkey != cov.connector_script {
            return Err(format!(
                "{o} is not the admin payout connector of its covenant"
            ));
        }
        if !node.unspent(&o, true).await? {
            return Err(format!("{o} is already spent"));
        }
    }
    Ok(())
}

/// Review of signing a pending transaction.
pub async fn review_sign(
    ctx: &PayoutContext,
    txid: &str,
    store: &PolicyStore,
    node: &dyn PayoutNode,
) -> Result<LedgerReviewDto, String> {
    let checked = fetch_checked_pending(&ctx.dep, txid, &ctx.coordinator, node).await?;
    check_app_unchanged(ctx).await?;
    ledger_review_for(
        &ctx.dep,
        &ctx.me,
        &ctx.app,
        &[checked.psbt],
        false,
        store,
        Some(checked.change_owner),
    )
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;
    use bitcoin::hashes::Hash;

    #[test]
    fn old_bitcoin_core_is_reported() {
        assert!(core_version_warning(300_200).is_none());
        assert!(core_version_warning(280_000).is_none());
        let w = core_version_warning(270_100).unwrap();
        assert!(w.starts_with("Bitcoin Core 27.1 is older than 28.0"), "{w}");
    }

    #[tokio::test]
    async fn a_pending_transaction_from_the_coordinator_is_checked_before_signing() {
        let (fx, dep, _me, psbt) = setup();
        let txid = psbt.unsigned_tx.compute_txid().to_string();
        let b64 = psbt.to_string();
        let check = |b64: String, txid: String, node: MockNode| {
            let dep = dep.clone();
            async move {
                check_pending_for_signing(&dep, &txid, &b64, &node)
                    .await
                    .map(|c| c.change_owner)
            }
        };
        // As built by admin 0: every input is on the node, the change is admin 0's.
        assert_eq!(
            check(b64.clone(), txid.clone(), node_for(&psbt)).await,
            Ok(0)
        );

        // The coordinator answers a request for another transaction with this one.
        let other = Txid::from_byte_array([9; 32]).to_string();
        let e = check(b64.clone(), other, node_for(&psbt))
            .await
            .unwrap_err();
        assert!(e.contains("instead of"), "{e}");

        // The output pays an address that is no admin's Admin Wallet.
        let mut stolen = psbt.clone();
        stolen.unsigned_tx.output[0].script_pubkey =
            ScriptBuf::new_p2tr(&Secp256k1::new(), fx.operator_xonly(0), None);
        let e = check(
            stolen.to_string(),
            stolen.unsigned_tx.compute_txid().to_string(),
            node_for(&stolen),
        )
        .await
        .unwrap_err();
        assert!(e.contains("not an Admin Wallet address"), "{e}");

        // The node knows the fee coin with another amount than the transaction states.
        let fee_outpoint = psbt.unsigned_tx.input[1].previous_output;
        let mut node = node_for(&psbt);
        node.outputs.get_mut(&fee_outpoint).unwrap().value = bitcoin::Amount::from_sat(1_000);
        let e = check(b64.clone(), txid.clone(), node).await.unwrap_err();
        assert!(e.contains("is not what this transaction says"), "{e}");

        // The node does not know an input at all, or it is already spent.
        let mut node = node_for(&psbt);
        node.outputs.remove(&fee_outpoint);
        let e = check(b64.clone(), txid.clone(), node).await.unwrap_err();
        assert!(e.contains("does not know"), "{e}");
        let mut node = node_for(&psbt);
        node.spent.insert(psbt.unsigned_tx.input[0].previous_output);
        let e = check(b64.clone(), txid.clone(), node).await.unwrap_err();
        assert!(e.contains("already spent"), "{e}");

        // A fee far above the highest rate the fee field accepts.
        let connector = ConnectorRef {
            claim_txid: Txid::from_byte_array([7; 32]),
            covenant_index: 0,
        };
        let burning = payout_tx::build_psbt(
            &dep,
            &[connector],
            &[fee_coin(&fx, 0, 0, 5_000_000)],
            &change(&fx, 1_000),
        )
        .unwrap();
        let e = check(
            burning.to_string(),
            burning.unsigned_tx.compute_txid().to_string(),
            node_for(&burning),
        )
        .await
        .unwrap_err();
        assert!(e.contains("above the highest fee rate"), "{e}");
    }
    #[tokio::test]
    async fn rebroadcast_sends_the_recorded_transaction_and_nothing_else() {
        let (fx, dep, _me, unsigned) = setup();
        let coin = fee_coin(&fx, 0, 0, 50_000);
        let mut signed = unsigned.clone();
        sign_as(&fx, &dep, &mut signed, 0);
        sign_as(&fx, &dep, &mut signed, 1);
        sign_fee(&fx, &mut signed, &coin, 0);
        let tx = payout_tx::finalize(&dep, &signed).unwrap();
        let raw = bitcoin::consensus::encode::serialize_hex(&tx);
        let txid = tx.compute_txid().to_string();
        let node = MockNode::default();

        // A payout completed outside the coordinator is archived with the PSBT it had then,
        // without the last signatures: the raw transaction is what gets sent.
        for psbt in [&signed, &unsigned] {
            assert_eq!(
                rebroadcast(&dep, &txid, &psbt.to_string(), &raw, &node).await,
                Ok(txid.clone())
            );
        }

        // A raw transaction that is not the payout of the record is not relayed.
        let mut other = tx.clone();
        other.output[0].value -= bitcoin::Amount::from_sat(1);
        let other_raw = bitcoin::consensus::encode::serialize_hex(&other);
        let e = rebroadcast(&dep, &txid, &unsigned.to_string(), &other_raw, &node)
            .await
            .unwrap_err();
        assert!(e.contains("was not sent"), "{e}");
        // Nor under another id than the row the admin clicked.
        let wrong = Txid::from_byte_array([9; 32]).to_string();
        assert!(
            rebroadcast(&dep, &wrong, &unsigned.to_string(), &raw, &node)
                .await
                .is_err()
        );
    }
}
