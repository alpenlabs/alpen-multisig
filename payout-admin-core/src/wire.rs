//! HTTP contract. JSON uses camelCase; all times are epoch seconds.
//! Except challenge and verify, routes require `Authorization: Bearer <sessionToken>`.
//! Route placeholders use Axum 0.7 syntax. Transaction IDs are hex, PSBTs base64,
//! raw transactions hex, and signature text is `txid:input:xonly:sig64` per line.
use bitcoin::{OutPoint, Txid, XOnlyPublicKey};
use serde::{Deserialize, Serialize};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The locally configured coordinator origin binds this sign-in to its intended server.
pub fn sign_in_message(origin: &str, nonce: &str) -> String {
    format!("Alpen payout admin sign-in\nCoordinator: {origin}\nNonce: {nonce}")
}

pub mod unix_seconds {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn seconds(time: SystemTime) -> Result<u64, std::time::SystemTimeError> {
        Ok(time.duration_since(UNIX_EPOCH)?.as_secs())
    }

    pub fn moment(seconds: u64) -> Result<SystemTime, &'static str> {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or("time out of range")
    }

    pub fn serialize<S: Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        seconds(*time)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        moment(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }

    pub mod optional {
        use super::*;

        pub fn serialize<S: Serializer>(
            time: &Option<SystemTime>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            time.map(seconds)
                .transpose()
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<SystemTime>, D::Error> {
            Option::<u64>::deserialize(deserializer)?
                .map(moment)
                .transpose()
                .map_err(serde::de::Error::custom)
        }
    }
}

mod token_hex {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(token: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(token))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let mut token = [0; 32];
        hex::decode_to_slice(String::deserialize(deserializer)?, &mut token)
            .map_err(serde::de::Error::custom)?;

        Ok(token)
    }
}

/// POST, no body -> ChallengeResponse. Nonces are single-use and expire.
pub const CHALLENGE: &str = "/api/v1/payout-admin/challenge";
/// POST VerifyRequest -> VerifyResponse. Recover the admin key via BIP-137.
pub const VERIFY: &str = "/api/v1/payout-admin/verify";
/// GET -> Vec<PendingBlockPayoutsView>; POST PsbtRequest -> PendingBlockPayoutsView.
pub const PENDING: &str = "/api/v1/payout-admin/pending";
/// POST SignaturesRequest -> PasteSignaturesView.
pub const SIGNATURES: &str = "/api/v1/payout-admin/pending/:txid/signatures";
/// POST ReportRequest -> (). Stores the report only if the transaction has none; 409 otherwise.
pub const REPORT: &str = "/api/v1/payout-admin/pending/:txid/report";
/// Largest report text the coordinator stores for one transaction, in bytes.
pub const MAX_REPORT_BYTES: usize = 32768;

/// A false claim report to attach to a pending transaction, as opaque text.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportRequest {
    pub report: String,
}

/// GET -> Vec<PastBlockPayoutsView>.
pub const PAST: &str = "/api/v1/payout-admin/past";
/// GET -> TransactionCopy. Pending returns PSBT; past returns signed raw tx.
pub const TRANSACTIONS: &str = "/api/v1/payout-admin/transactions/:txid";
/// POST, no body -> BroadcastResponse. Retry pending or rebroadcast past.
pub const BROADCAST: &str = "/api/v1/payout-admin/broadcast/:txid";

/// The coordinator's single-use sign-in challenge and its deadline.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    /// Sign this exact text as a Bitcoin message.
    pub nonce: String,
    /// Challenge deadline in epoch seconds.
    #[serde(with = "unix_seconds", rename = "expiresAtUnixSeconds")]
    pub expires_at: SystemTime,
}

/// Proof of control of an admin key for a previously issued challenge.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifyRequest {
    pub nonce: String,
    /// Base64 compact BIP-137 Bitcoin signed-message signature of the nonce verbatim.
    pub signature: String,
}

/// The authenticated admin key and the credentials for subsequent coordinator requests.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    /// Bearer credential for authenticated payout routes.
    #[serde(with = "token_hex")]
    pub session_token: [u8; 32],
    /// Session deadline in epoch seconds.
    #[serde(with = "unix_seconds", rename = "expiresAtUnixSeconds")]
    pub expires_at: SystemTime,
    /// X-only deployment key recovered from the challenge signature.
    pub admin_key: XOnlyPublicKey,
}

/// Submits an unfinalized block payouts PSBT for collection or merging of signatures.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PsbtRequest {
    /// Base64 encoding of the PSBT.
    pub psbt: String,
    pub report: Option<String>,
}

/// Submits pasted admin signatures for the transaction identified by the route.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignaturesRequest {
    /// One line per signature: `txid:input:xonly:sig64`.
    pub signatures: String,
}

/// A pending PSBT with its signing progress, expiry, and conflicts with other pending spends.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBlockPayoutsView {
    /// Signatures newly accepted by this submission (zero for a list response).
    #[serde(default)]
    pub added: usize,
    pub txid: Txid,
    /// Base64 PSBT including the signatures collected so far.
    pub psbt: String,
    pub report: Option<String>,
    /// Pending transaction deadline in epoch seconds.
    #[serde(with = "unix_seconds", rename = "expiresAtUnixSeconds")]
    pub expires_at: SystemTime,
    /// Number of admin payout connectors spent by the PSBT.
    pub claim_input_count: usize,
    /// Admins who signed every claim input, not total input signatures.
    pub admins_signed: usize,
    /// Total signature threshold per connector input.
    pub signatures_needed: usize,
    /// X-only keys of those admins, for the current admin's Signed marker.
    pub signed_by: Vec<XOnlyPublicKey>,
    /// Claim or fee outpoints also used by another pending transaction.
    pub duplicate_outpoints: Vec<OutPoint>,
    pub broadcast_error: Option<String>,
}

/// Confirmation state of a previously broadcast block payouts.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PastStatus {
    Unconfirmed,
    Confirmed,
}

/// A broadcast transaction and the coordinator's record of its confirmation state.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastBlockPayoutsView {
    pub txid: Txid,
    /// Hex serialization of the signed transaction, including witnesses.
    pub raw_tx: String,
    pub status: PastStatus,
    /// Timestamp of the containing block in epoch seconds, absent while unconfirmed.
    #[serde(
        default,
        with = "unix_seconds::optional",
        rename = "blockTimeUnixSeconds"
    )]
    pub block_time: Option<SystemTime>,
}

/// Clipboard representation of a pending PSBT or a previously broadcast transaction.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "format", content = "data", rename_all = "camelCase")]
pub enum TransactionCopy {
    Psbt(String),
    RawTx(String),
}

/// The coordinator's per-paste counts, rejected items, and remaining pending state.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteSignaturesView {
    /// None if the added signatures triggered a successful broadcast.
    pub pending: Option<PendingBlockPayoutsView>,
    /// Number of signatures newly added to the PSBT.
    pub added: usize,
    /// Number of submitted signatures counted as duplicates.
    pub already_present: usize,
    pub invalid: Vec<crate::block_payouts_tx::InvalidSignature>,
}

/// Result of a coordinator broadcast request for the identified block payouts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastResponse {
    pub txid: Txid,
    /// Broadcast error stored on the pending record, if any.
    pub broadcast_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_time_before_epoch() {
        let before_epoch = UNIX_EPOCH.checked_sub(Duration::from_secs(1)).unwrap();
        assert!(unix_seconds::seconds(before_epoch).is_err());
    }

    #[test]
    fn rejects_short_session_token() {
        let response = serde_json::json!({
            "sessionToken": "07".repeat(31),
            "expiresAtUnixSeconds": 123,
            "adminKey": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        });
        assert!(serde_json::from_value::<VerifyResponse>(response).is_err());
    }

    #[test]
    fn json_contract_uses_camel_case_and_null_unconfirmed_time() {
        let past = PastBlockPayoutsView {
            txid: "ab".repeat(32).parse().unwrap(),
            raw_tx: "raw".into(),
            status: PastStatus::Unconfirmed,
            block_time: None,
        };
        let value = serde_json::to_value(past).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"txid":"ab".repeat(32), "rawTx":"raw", "status":"unconfirmed", "blockTimeUnixSeconds":null})
        );
        let key = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
        let auth = serde_json::to_value(VerifyResponse {
            session_token: [7; 32],
            expires_at: unix_seconds::moment(123).unwrap(),
            admin_key: key.parse().unwrap(),
        })
        .unwrap();
        assert_eq!(auth["sessionToken"], "07".repeat(32));
        assert_eq!(auth["adminKey"], key);
        assert_eq!(auth["expiresAtUnixSeconds"], 123);
    }
}
