//! What the Payout Administrator screens read from the admin's own Bitcoin Core node. Per the
//! design, these screens talk to Bitcoin Core only (never to the ASM or a bridge node).

use async_trait::async_trait;
use bitcoin::{consensus::encode, OutPoint, Transaction, Txid};
use serde_json::{json, Value};

use crate::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;

/// A transaction as the node knows it.
#[derive(Debug, Clone)]
pub struct NodeTx {
    pub tx: Transaction,
    /// `None` while it is in the mempool.
    pub block_height: Option<u64>,
    pub block_time: Option<u64>,
    pub confirmations: u64,
}

#[async_trait]
pub trait PayoutNode: Send + Sync {
    async fn tx(&self, txid: &Txid) -> Result<Option<NodeTx>, String>;
    async fn unspent(&self, outpoint: &OutPoint, include_mempool: bool) -> Result<bool, String>;
    async fn tip(&self) -> Result<u64, String>;
}
const RPC_NOT_FOUND: i64 = -5;
#[async_trait]
impl PayoutNode for HttpBitcoinRpcClient {
    async fn tx(&self, txid: &Txid) -> Result<Option<NodeTx>, String> {
        let v = match self
            .rpc("getrawtransaction", json!([txid.to_string(), true]))
            .await
        {
            Ok(v) => v,
            Err(e) if e.code == Some(RPC_NOT_FOUND) => return Ok(None),
            Err(e) => return Err(e.message),
        };
        let hex = v
            .get("hex")
            .and_then(Value::as_str)
            .ok_or("getrawtransaction: no hex")?;
        let tx: Transaction =
            encode::deserialize_hex(hex).map_err(|e| format!("getrawtransaction: {e}"))?;
        if tx.compute_txid() != *txid {
            return Err(format!(
                "the node returned a different transaction for {txid}"
            ));
        }
        let confirmations = v.get("confirmations").and_then(Value::as_u64).unwrap_or(0);
        let block_time = v.get("blocktime").and_then(Value::as_u64);
        let block_height = match v.get("blockhash").and_then(Value::as_str) {
            Some(hash) if confirmations > 0 => {
                let h = self
                    .rpc("getblockheader", json!([hash, true]))
                    .await
                    .map_err(|e| e.message)?;
                h.get("height").and_then(Value::as_u64)
            }
            _ => None,
        };
        Ok(Some(NodeTx {
            tx,
            block_height,
            block_time,
            confirmations,
        }))
    }
    async fn unspent(&self, outpoint: &OutPoint, include_mempool: bool) -> Result<bool, String> {
        let v = self
            .rpc(
                "gettxout",
                json!([outpoint.txid.to_string(), outpoint.vout, include_mempool]),
            )
            .await
            .map_err(|e| e.message)?;
        Ok(!v.is_null())
    }
    async fn tip(&self) -> Result<u64, String> {
        self.rpc("getblockcount", json!([]))
            .await
            .map_err(|e| e.message)?
            .as_u64()
            .ok_or_else(|| "getblockcount: expected a number".to_string())
    }
}
