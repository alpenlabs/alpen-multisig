use super::{AppError, HttpBitcoinRpcClient, TxIndex};
use serde_json::json;

impl HttpBitcoinRpcClient {
    pub(super) async fn payout_out_unspent(
        &self,
        txid: &str,
        vout: u32,
        include_mempool: bool,
    ) -> Result<bool, AppError> {
        let out = self
            .call("gettxout", json!([txid, vout, include_mempool]))
            .await?;
        Ok(!out.is_null())
    }

    pub(super) async fn payout_accept(&self, tx: &bitcoin::Transaction) -> Result<(), AppError> {
        let raw = bitcoin::consensus::encode::serialize_hex(tx);
        match self.call("sendrawtransaction", json!([raw])).await {
            Ok(value) if value.as_str() == Some(&tx.compute_txid().to_string()) => Ok(()),
            Ok(_) => Err(AppError::BadRequest(
                "node returned an unexpected payout txid".to_string(),
            )),
            Err(AppError::BadRequest(m))
                if m.contains("already in block chain")
                    || m.contains("txn-already-in-mempool")
                    || m.contains("txn-already-known") =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub(super) async fn payout_read_transaction(
        &self,
        txid: &str,
    ) -> Result<Option<bitcoin::Transaction>, AppError> {
        match self.call("getrawtransaction", json!([txid, false])).await {
            Ok(raw) => {
                let raw = raw.as_str().ok_or_else(|| {
                    AppError::BadRequest("node returned invalid raw transaction".to_string())
                })?;
                bitcoin::consensus::encode::deserialize_hex(raw)
                    .map(Some)
                    .map_err(|e| AppError::BadRequest(format!("invalid node transaction: {e}")))
            }
            Err(AppError::BadRequest(m)) if m.contains("No such mempool") => {
                if self.txindex().await? != TxIndex::Synced {
                    return Err(AppError::BadRequest(
                        "payout history is unavailable without a synced txindex; pending payout retained".to_string(),
                    ));
                }
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
