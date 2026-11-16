use super::*;

impl HttpBitcoinRpcClient {
    /// Any node RPC, keeping how it failed. Used by the payout views (`gettxout`,
    /// `getrawtransaction` verbose, `getblockheader`).
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_typed(method, params, None).await
    }

    /// [`Self::rpc`] for a call the node may take long to answer (`scantxoutset` walks the whole
    /// UTXO set): `timeout` replaces the standard RPC timeout for this one request.
    pub async fn rpc_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, RpcError> {
        self.call_typed(method, params, Some(timeout)).await
    }
}
