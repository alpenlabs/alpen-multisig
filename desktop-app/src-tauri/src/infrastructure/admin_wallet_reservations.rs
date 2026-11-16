//! Coins of the Admin Wallet that one part of the app has committed, so that the other part does
//! not spend them.
//!
//! Block Payouts and the Admin Wallet panel use the same wallet (the same Ledger account), but
//! read it from different sources and keep their own state. A fee coin of a payout that waits for
//! signatures is unspent on every source, so only the app can know that it is taken.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use bitcoin::OutPoint;

const FILE: &str = "payout-reserved-coins.json";

/// A pending payout lives 4 days. No coin is held longer than that on the coordinator's word.
const MAX_HOLD_SECS: i64 = 4 * 24 * 60 * 60;

#[derive(Clone, Default)]
pub struct AdminWalletReservations(Arc<Inner>);

#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    /// Coins spent by pending payouts, each with the unix time until which it is held. Kept on
    /// disk: the Admin Wallet panel also works without a payout session.
    payouts: Mutex<HashMap<OutPoint, i64>>,
    /// Coins spent by transactions of the Admin Wallet panel that no sync has seen yet.
    in_flight: Mutex<HashSet<OutPoint>>,
    own_sends: Mutex<HashSet<OutPoint>>,
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// Every critical section is a plain map update, so a poisoned lock holds valid data.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AdminWalletReservations {
    /// `dir` is the app data dir; `None` keeps everything in memory only (tests).
    pub fn new(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join(FILE));
        let stored: HashMap<String, i64> = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let payouts = stored
            .into_iter()
            .filter_map(|(k, until)| Some((OutPoint::from_str(&k).ok()?, until)))
            .collect();
        Self(Arc::new(Inner {
            path,
            payouts: Mutex::new(payouts),
            in_flight: Mutex::default(),
            own_sends: Mutex::default(),
        }))
    }

    /// Replaces the coins held by pending payouts: `(coin, unix time the payout expires)`.
    pub fn set_held_by_payouts(&self, coins: impl IntoIterator<Item = (OutPoint, i64)>, now: i64) {
        let mut next: HashMap<OutPoint, i64> = HashMap::new();
        for (coin, expires_at) in coins {
            let until = expires_at.min(now + MAX_HOLD_SECS);
            // A coin in two pending payouts is held as long as the later one lives.
            let held = next.entry(coin).or_insert(until);
            *held = (*held).max(until);
        }
        let mut payouts = lock(&self.0.payouts);
        if *payouts == next {
            return;
        }
        *payouts = next;
        self.store(&payouts);
    }

    /// Coins a pending payout spends. A hold ends when its payout expires, also when this app
    /// could not ask the coordinator since.
    pub fn held_by_payouts(&self, now: i64) -> HashSet<OutPoint> {
        lock(&self.0.payouts)
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(coin, _)| *coin)
            .collect()
    }

    /// Replaces the coins spent by in-flight transactions of the Admin Wallet panel.
    pub fn set_in_flight(&self, coins: impl IntoIterator<Item = OutPoint>) {
        *lock(&self.0.in_flight) = coins.into_iter().collect();
    }

    pub fn in_flight(&self) -> HashSet<OutPoint> {
        lock(&self.0.in_flight).clone()
    }

    pub fn set_own_sends(&self, coins: impl IntoIterator<Item = OutPoint>) {
        *lock(&self.0.own_sends) = coins.into_iter().collect();
    }
    pub fn own_sends(&self) -> HashSet<OutPoint> {
        lock(&self.0.own_sends).clone()
    }

    // A write that fails costs the holds after a restart only; the next overview of the payout
    // screen brings them back.
    fn store(&self, payouts: &HashMap<OutPoint, i64>) {
        let Some(path) = &self.0.path else {
            return;
        };
        let stored: HashMap<String, i64> = payouts
            .iter()
            .map(|(coin, until)| (coin.to_string(), *until))
            .collect();
        let written = serde_json::to_string_pretty(&stored)
            .map_err(|e| e.to_string())
            .and_then(|json| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(path, json).map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            tracing::warn!("coins held by pending payouts were not saved: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coin(vout: u32) -> OutPoint {
        OutPoint::new(
            "4444444444444444444444444444444444444444444444444444444444444444"
                .parse()
                .unwrap(),
            vout,
        )
    }

    #[test]
    fn a_hold_ends_when_the_payout_expires() {
        let r = AdminWalletReservations::default();
        r.set_held_by_payouts([(coin(0), 1_000), (coin(1), 2_000)], 500);
        assert_eq!(r.held_by_payouts(999), HashSet::from([coin(0), coin(1)]));
        assert_eq!(r.held_by_payouts(1_000), HashSet::from([coin(1)]));
        assert!(r.held_by_payouts(2_000).is_empty());
    }

    #[test]
    fn a_new_list_replaces_the_old_one() {
        let r = AdminWalletReservations::default();
        r.set_held_by_payouts([(coin(0), 1_000)], 0);
        r.set_held_by_payouts([(coin(1), 1_000)], 0);
        assert_eq!(r.held_by_payouts(0), HashSet::from([coin(1)]));
    }

    #[test]
    fn no_coin_is_held_longer_than_a_payout_lives() {
        let r = AdminWalletReservations::default();
        r.set_held_by_payouts([(coin(0), i64::MAX)], 100);
        assert!(r
            .held_by_payouts(100 + MAX_HOLD_SECS - 1)
            .contains(&coin(0)));
        assert!(r.held_by_payouts(100 + MAX_HOLD_SECS).is_empty());
    }

    #[test]
    fn a_coin_in_two_payouts_is_held_until_the_later_one_expires() {
        let r = AdminWalletReservations::default();
        r.set_held_by_payouts([(coin(0), 2_000), (coin(0), 1_000)], 0);
        assert!(r.held_by_payouts(1_500).contains(&coin(0)));
    }

    #[test]
    fn holds_survive_a_restart_and_in_flight_coins_do_not() {
        let dir =
            std::env::temp_dir().join(format!("admin-wallet-reservations-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = AdminWalletReservations::new(Some(dir.clone()));
        r.set_held_by_payouts([(coin(0), 1_000)], 0);
        r.set_in_flight([coin(1)]);
        assert_eq!(r.in_flight(), HashSet::from([coin(1)]));

        let again = AdminWalletReservations::new(Some(dir.clone()));
        assert_eq!(again.held_by_payouts(0), HashSet::from([coin(0)]));
        assert!(again.in_flight().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
