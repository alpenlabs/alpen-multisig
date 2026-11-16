use super::{
    payout_tx, sign_connectors, sign_fee_inputs, ClaimInput, CoordinatorPending, Deployment,
    PayoutContext, PayoutCoordinator, PayoutNode, PlanClaim, PolicyStore, Psbt, ScriptBuf,
    Serialize,
};
use std::str::FromStr;
pub(super) fn address_on_device(dep: &Deployment, script: &ScriptBuf) -> Option<String> {
    (dep.network == bitcoin::Network::Regtest)
        .then(|| bitcoin::Address::from_script(script, bitcoin::Network::Testnet).ok())
        .flatten()
        .map(|a| a.to_string())
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
