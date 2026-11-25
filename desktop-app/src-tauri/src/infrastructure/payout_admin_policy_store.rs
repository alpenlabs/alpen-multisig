//! Stores the wallet registration codes returned by a Ledger.

use bitcoin::bip32::Fingerprint;
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

const FILE: &str = "payout-admin-policies.json";
type Registrations = HashMap<(Fingerprint, [u8; 32]), [u8; 32]>;

/// Keeps registration codes so the admin approves each wallet registration only once.
pub struct PolicyStore {
    path: Option<PathBuf>,
    /// The key identifies the seed by its Ledger master fingerprint (the same on another
    /// Ledger with that seed) and the admin multisig wallet by its policy ID. The value
    /// is the non-secret 32-byte HMAC returned on approval and sent with each signing
    /// request. The file uses hex for both key components and the code. Wrong lengths
    /// are rejected when reading.
    entries: Mutex<Registrations>,
}

impl PolicyStore {
    /// Loads payout-admin-policies.json from the app data folder, or uses memory alone.
    pub fn new(directory: Option<PathBuf>) -> Self {
        let path = directory.map(|directory| directory.join(FILE));
        let entries = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<HashMap<String, String>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, code)| {
                let (fingerprint, policy) = key.split_once(':')?;

                Some((
                    (
                        fingerprint.parse().ok()?,
                        hex::decode(policy).ok()?.try_into().ok()?,
                    ),
                    hex::decode(code).ok()?.try_into().ok()?,
                ))
            })
            .collect();

        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    /// Returns the registration code for this Ledger seed and wallet policy.
    pub fn get(&self, fingerprint: Fingerprint, policy_id: &[u8; 32]) -> Option<[u8; 32]> {
        self.entries
            .lock()
            .ok()?
            .get(&(fingerprint, *policy_id))
            .copied()
    }

    /// Records the approved registration and writes it to the configured app data folder.
    pub fn save(
        &self,
        fingerprint: Fingerprint,
        policy_id: &[u8; 32],
        hmac: &[u8; 32],
    ) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "policy store lock poisoned")?;
        entries.insert((fingerprint, *policy_id), *hmac);
        let Some(path) = &self.path else {
            return Ok(());
        };

        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| format!("policy store: {error}"))?;
        }

        let encoded: HashMap<_, _> = entries
            .iter()
            .map(|((fingerprint, policy), code)| {
                (
                    format!("{fingerprint}:{}", hex::encode(policy)),
                    hex::encode(code),
                )
            })
            .collect();
        let json = serde_json::to_string_pretty(&encoded).map_err(|error| error.to_string())?;

        std::fs::write(path, json).map_err(|error| format!("policy store: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_by_device_and_policy() {
        let directory =
            std::env::temp_dir().join(format!("payout-policy-store-{}", std::process::id()));
        let store = PolicyStore::new(Some(directory.clone()));
        let fingerprint = Fingerprint::from([7; 4]);
        store.save(fingerprint, &[1; 32], &[9; 32]).unwrap();
        assert_eq!(
            PolicyStore::new(Some(directory.clone())).get(fingerprint, &[1; 32]),
            Some([9; 32])
        );
        assert_eq!(store.get(Fingerprint::from([8; 4]), &[1; 32]), None);
        let memory_store = PolicyStore::new(None);
        memory_store.save(fingerprint, &[2; 32], &[3; 32]).unwrap();
        assert_eq!(memory_store.get(fingerprint, &[2; 32]), Some([3; 32]));
        let _ = std::fs::remove_dir_all(directory);
    }
}
