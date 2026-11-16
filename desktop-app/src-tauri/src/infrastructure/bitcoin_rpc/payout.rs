use super::*;

impl HttpBitcoinRpcClient {
    /// Any node RPC, keeping how it failed. Used by the payout views (`gettxout`,
    /// `getrawtransaction` verbose, `getblockheader`).
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_typed(method, params, None).await
    }
}
