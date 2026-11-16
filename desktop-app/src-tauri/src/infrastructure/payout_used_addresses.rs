//! Which Admin Wallet addresses were already used, remembered across restarts.
//!
//! The payout screens read the wallet from Bitcoin Core's UTXO set, which shows an address only
//! while it holds a coin. Once the coin is spent the address would look unused again and be
//! offered a second time. This keeps, per wallet and branch, the first index that was never seen
//! with a coin; addresses below it are not handed out again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const FILE: &str = "payout-used-addresses.json";

/// `[receive, change]`: the lowest index of each branch that may still be handed out.
pub type Floors = [u32; 2];

#[derive(Clone, Default)]
pub struct UsedAddresses(Arc<Inner>);

#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    // Also the lock for the file: every change reads the file again, so a second running app
    // does not lose what the first one wrote.
    memory: Mutex<HashMap<String, Floors>>,
}

impl UsedAddresses {
    /// `dir` is the app data dir; `None` keeps everything in memory only (tests).
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            path: dir.map(|d| d.join(FILE)),
            memory: Mutex::default(),
        }))
    }

    /// Records that `branch` was seen in use at each of `indexes`, and returns the floors of
    /// `wallet`. A file that cannot be read counts as empty: the cost is an address offered
    /// again, which the current coins still prevent for every address that holds funds.
    pub fn seen(&self, wallet: &str, used: impl IntoIterator<Item = (u32, u32)>) -> Floors {
        let mut memory = self.0.memory.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = &self.0.path {
            if let Some(stored) = std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                *memory = stored;
            }
        }
        let before = memory.get(wallet).copied().unwrap_or_default();
        let mut floors = before;
        for (branch, index) in used {
            if let Some(floor) = floors.get_mut(branch as usize) {
                *floor = (*floor).max(index.saturating_add(1));
            }
        }
        if floors != before {
            memory.insert(wallet.to_string(), floors);
            if let Some(path) = &self.0.path {
                let written = serde_json::to_vec_pretty(&*memory)
                    .map_err(|e| e.to_string())
                    .and_then(|json| {
                        if let Some(dir) = path.parent() {
                            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                        }
                        std::fs::write(path, json).map_err(|e| e.to_string())
                    });
                if let Err(e) = written {
                    tracing::warn!("used Admin Wallet addresses were not saved: {e}");
                }
            }
        }
        floors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_once_seen_in_use_stays_used() {
        let used = UsedAddresses::default();
        assert_eq!(used.seen("w", []), [0, 0]);
        assert_eq!(used.seen("w", [(0, 2), (1, 0)]), [3, 1]);
        // The coins are spent: nothing is seen any more, the floors stay.
        assert_eq!(used.seen("w", []), [3, 1]);
        assert_eq!(used.seen("w", [(0, 0)]), [3, 1]);
        assert_eq!(used.seen("another wallet", []), [0, 0]);
    }

    #[test]
    fn floors_survive_a_restart() {
        let dir =
            std::env::temp_dir().join(format!("payout-used-addresses-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        UsedAddresses::new(Some(dir.clone())).seen("w", [(0, 4)]);
        assert_eq!(UsedAddresses::new(Some(dir.clone())).seen("w", []), [5, 0]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
