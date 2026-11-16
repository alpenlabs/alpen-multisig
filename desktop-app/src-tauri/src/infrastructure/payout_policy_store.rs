//! Ledger policy HMACs for the payout multisig, kept in the app data dir
//! (`payout-policies.json`), keyed by device fingerprint and BIP-388 policy id.
//!
//! An HMAC is not secret (it only lets that one device skip re-reviewing a policy it already
//! approved) but it is device- and policy-specific.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

const FILE: &str = "payout-policies.json";

pub struct PolicyStore {
    path: Option<PathBuf>,
    entries: Mutex<HashMap<String, String>>,
}

fn key(fingerprint: u32, policy_id: &[u8; 32]) -> String {
    format!("{fingerprint:08x}:{}", hex::encode(policy_id))
}

impl PolicyStore {
    /// `dir` is the app data dir; `None` keeps HMACs in memory only (tests).
    pub fn new(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join(FILE));
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn get(&self, fingerprint: u32, policy_id: &[u8; 32]) -> Option<[u8; 32]> {
        let v = self
            .entries
            .lock()
            .ok()?
            .get(&key(fingerprint, policy_id))?
            .clone();
        hex::decode(v).ok()?.try_into().ok()
    }

    pub fn save(
        &self,
        fingerprint: u32,
        policy_id: &[u8; 32],
        hmac: &[u8; 32],
    ) -> Result<(), String> {
        let mut all = self
            .entries
            .lock()
            .map_err(|_| "policy store lock poisoned")?;
        all.insert(key(fingerprint, policy_id), hex::encode(hmac));
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("policy store: {e}"))?;
        }
        let json = serde_json::to_string_pretty(&*all).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("policy store: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_by_device_and_policy() {
        let dir = std::env::temp_dir().join(format!("payout-policy-store-{}", std::process::id()));
        let s = PolicyStore::new(Some(dir.clone()));
        s.save(7, &[1; 32], &[9; 32]).unwrap();
        assert_eq!(
            PolicyStore::new(Some(dir.clone())).get(7, &[1; 32]),
            Some([9; 32])
        );
        assert_eq!(s.get(8, &[1; 32]), None);
        let mem = PolicyStore::new(None);
        mem.save(1, &[2; 32], &[3; 32]).unwrap();
        assert_eq!(mem.get(1, &[2; 32]), Some([3; 32]));
        let _ = std::fs::remove_dir_all(dir);
    }
}
