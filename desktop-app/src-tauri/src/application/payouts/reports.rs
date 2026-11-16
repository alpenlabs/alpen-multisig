use super::{
    report, BTreeSet, ConnectorState, Deployment, HashMap, HashSet, MinedTx, OutPoint, PayoutNode,
    Serialize, Txid, VerifiedReport,
};

// Only the UI projection is flattened; the verified report retains all its evidence.
fn report_projection<S: serde::Serializer>(
    report: &VerifiedReport,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut value = serde_json::to_value(report).map_err(serde::ser::Error::custom)?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("report");
        fields.insert(
            "newClaimTxid".into(),
            serde_json::json!(report.report.new_claim_txid),
        );
    }
    value.serialize(serializer)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportDto {
    #[serde(flatten, serialize_with = "report_projection")]
    pub verified: VerifiedReport,
    /// Already an input of a pending transaction.
    pub in_pending: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReportsDto {
    pub reports: Vec<ReportDto>,
    pub malformed: Vec<report::MalformedReport>,
}

/// Parses pasted reports and verifies each one against the node. Invalid ones and ones whose
/// connector is already spent in a block come back marked, to be ignored.
pub async fn verify_reports(
    dep: &Deployment,
    text: &str,
    node: &dyn PayoutNode,
    pending_outpoints: &HashSet<OutPoint>,
) -> Result<VerifyReportsDto, String> {
    let parsed = report::parse_paste(text, dep.network).map_err(|e| e.to_string())?;
    let mut txs: HashMap<Txid, MinedTx> = HashMap::new();
    let wanted: BTreeSet<Txid> = parsed.reports.iter().flat_map(|r| r.txids()).collect();
    for txid in wanted {
        if let Some(t) = node.tx(&txid).await? {
            txs.insert(
                txid,
                MinedTx {
                    tx: t.tx,
                    block_height: t.block_height,
                    confirmations: t.confirmations,
                },
            );
        }
    }
    let mut states = HashMap::new();
    for r in &parsed.reports {
        let o = r.blocked_outpoint();
        if txs.contains_key(&o.txid) && !states.contains_key(&o) {
            let s = if node.unspent(&o, false).await? {
                ConnectorState::Unspent
            } else {
                ConnectorState::SpentConfirmed
            };
            states.insert(o, s);
        }
    }
    node.tip().await?;
    let reports = report::verify_reports(dep, &parsed.reports, &txs, &states)
        .into_iter()
        .map(|v| ReportDto {
            in_pending: pending_outpoints.contains(&v.outpoint),
            verified: v,
        })
        .collect();
    Ok(VerifyReportsDto {
        reports,
        malformed: parsed.malformed,
    })
}
