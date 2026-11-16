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

pub struct MempoolPackage {
    pub fee_sat: u64,
    pub vsize: u64,
}

#[async_trait]
pub trait PayoutNode: Send + Sync {
    /// `None` when the node does not know the transaction (needs `-txindex` for mined ones).
    async fn tx(&self, txid: &Txid) -> Result<Option<NodeTx>, String>;
    /// Whether the output exists and is unspent; mempool spends count only with `include_mempool`.
    async fn unspent(&self, outpoint: &OutPoint, include_mempool: bool) -> Result<bool, String>;
    /// `sendrawtransaction`; the error is the node's message.
    async fn broadcast(&self, tx: &Transaction) -> Result<Txid, String>;
    async fn tip(&self) -> Result<u64, String>;
    /// Bitcoin Core's version number (`getnetworkinfo`), e.g. 300200 for 30.2.0.
    async fn version(&self) -> Result<u64, String>;
    async fn fee_rate(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn incremental_relay_tenths(&self) -> Result<u64, String> {
        Ok(10)
    }
    async fn mempool_package(&self, _txid: &Txid) -> Result<Option<MempoolPackage>, String> {
        Ok(None)
    }
    async fn in_mempool(&self, txid: &Txid) -> Result<bool, String> {
        Ok(self.tx(txid).await?.is_some_and(|t| t.confirmations == 0))
    }
    /// Confirmed outputs matching the descriptors (`scantxoutset`, chainstate only).
    async fn scan(&self, descriptors: &[String]) -> Result<Vec<ScannedCoin>, String>;
}

/// One `scantxoutset` result.
#[derive(Debug, Clone)]
pub struct ScannedCoin {
    pub outpoint: OutPoint,
    pub value_sat: u64,
    pub script_pubkey: bitcoin::ScriptBuf,
    /// The inferred descriptor, with the key origin (`…/branch/index`).
    pub desc: String,
    pub height: u64,
}

/// `(branch, index)` from a `scantxoutset` descriptor like `tr([fp/86h/1h/73h/1/5]…)#…`.
pub fn branch_index_of(desc: &str) -> Option<(u32, u32)> {
    let origin = desc.split('[').nth(1)?.split(']').next()?;
    let mut parts = origin.rsplit('/');
    let index = parts.next()?.parse().ok()?;
    let branch = parts.next()?.parse().ok()?;
    Some((branch, index))
}

/// Bitcoin Core's "not found" code for getrawtransaction (`RPC_INVALID_ADDRESS_OR_KEY`).
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

    async fn broadcast(&self, tx: &Transaction) -> Result<Txid, String> {
        let v = self
            .rpc("sendrawtransaction", json!([encode::serialize_hex(tx)]))
            .await
            .map_err(|e| e.message)?;
        v.as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| "sendrawtransaction: expected a txid".to_string())
    }

    async fn tip(&self) -> Result<u64, String> {
        self.rpc("getblockcount", json!([]))
            .await
            .map_err(|e| e.message)?
            .as_u64()
            .ok_or_else(|| "getblockcount: expected a number".to_string())
    }

    async fn version(&self) -> Result<u64, String> {
        self.rpc("getnetworkinfo", json!([]))
            .await
            .map_err(|e| e.message)?["version"]
            .as_u64()
            .ok_or_else(|| "getnetworkinfo: expected a version number".to_string())
    }

    async fn fee_rate(&self) -> Result<Option<String>, String> {
        let value = self
            .rpc("estimatesmartfee", json!([1]))
            .await
            .map_err(|e| e.message)?;
        Ok(value["feerate"].as_f64().map(|v| {
            let tenths = (v * 1_000_000.0).ceil().max(1.0) as u64;
            format!("{}.{:01}", tenths / 10, tenths % 10)
        }))
    }
    async fn incremental_relay_tenths(&self) -> Result<u64, String> {
        let value = self
            .rpc("getmempoolinfo", json!([]))
            .await
            .map_err(|e| e.message)?;
        Ok(value["incrementalrelayfee"]
            .as_f64()
            .map(|v| (v * 1_000_000.0).ceil() as u64)
            .unwrap_or(10))
    }
    async fn mempool_package(&self, txid: &Txid) -> Result<Option<MempoolPackage>, String> {
        let value = match self.rpc("getmempoolentry", json!([txid.to_string()])).await {
            Ok(value) => value,
            Err(e) if e.code == Some(-5) => return Ok(None),
            Err(e) => return Err(e.message),
        };
        let fee = value["fees"]["descendant"]
            .as_f64()
            .ok_or("getmempoolentry: missing descendant fee")?;
        Ok(Some(MempoolPackage {
            fee_sat: bitcoin::Amount::from_btc(fee)
                .map_err(|e| e.to_string())?
                .to_sat(),
            vsize: value["descendantsize"]
                .as_u64()
                .ok_or("getmempoolentry: missing descendant size")?,
        }))
    }
    async fn in_mempool(&self, txid: &Txid) -> Result<bool, String> {
        match self.rpc("getmempoolentry", json!([txid.to_string()])).await {
            Ok(_) => Ok(true),
            Err(e) if e.code == Some(-5) => Ok(false),
            Err(e) => Err(e.message),
        }
    }

    async fn scan(&self, descriptors: &[String]) -> Result<Vec<ScannedCoin>, String> {
        let objs: Vec<Value> = descriptors
            .iter()
            .map(|d| json!({ "desc": d, "range": SCAN_RANGE }))
            .collect();
        let v = self
            .rpc_with_timeout("scantxoutset", json!(["start", objs]), SCAN_TIMEOUT)
            .await
            .map_err(|e| e.message)?;
        let unspents = v
            .get("unspents")
            .and_then(Value::as_array)
            .ok_or("scantxoutset: no unspents")?;
        unspents
            .iter()
            .map(|u| {
                let field = |k: &str| u.get(k).ok_or(format!("scantxoutset: no {k}"));
                let txid: Txid = field("txid")?
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or("scantxoutset: bad txid")?;
                let vout = field("vout")?.as_u64().ok_or("scantxoutset: bad vout")? as u32;
                let amount = field("amount")?
                    .as_f64()
                    .ok_or("scantxoutset: bad amount")?;
                let value_sat = bitcoin::Amount::from_btc(amount)
                    .map_err(|e| format!("scantxoutset: {e}"))?
                    .to_sat();
                let script = field("scriptPubKey")?
                    .as_str()
                    .and_then(|s| bitcoin::ScriptBuf::from_hex(s).ok())
                    .ok_or("scantxoutset: bad scriptPubKey")?;
                Ok(ScannedCoin {
                    outpoint: OutPoint::new(txid, vout),
                    value_sat,
                    script_pubkey: script,
                    desc: field("desc")?.as_str().unwrap_or_default().to_string(),
                    height: field("height")?.as_u64().unwrap_or(0),
                })
            })
            .collect()
    }
}

/// Address indexes scanned per branch of the Admin Wallet.
pub const SCAN_RANGE: u32 = 1_000;

/// How long `scantxoutset` may take. It reads the whole UTXO set, which on mainnet takes well over
/// the 30 seconds other RPC calls get.
pub const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

#[cfg(test)]
mod tests {
    #[test]
    fn branch_index_from_descriptor() {
        assert_eq!(
            super::branch_index_of("tr([0a1b2c3d/86h/1h/73h/1/5]03ab)#xyz"),
            Some((1, 5))
        );
        assert_eq!(super::branch_index_of("raw(00)"), None);
    }
}
