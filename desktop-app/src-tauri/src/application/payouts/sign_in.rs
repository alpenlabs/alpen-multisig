use super::PayoutDevice;
use super::{
    account_path, admin_label, blocking, check_coordinator_deployment, fingerprint_u32, text,
    AdminDto, AdminSigner, AdminWalletReservations, Arc, CoinCache, Deployment, FromStr,
    LedgerPayoutDevice, LedgerReviewDto, LedgerStepDto, PayoutContext, PayoutCoordinator,
    PayoutState, PolicyStore, Serialize, SessionInner, SignatureResult, UsedAddresses,
    XOnlyPublicKey, Xpub, MSG_NOT_A_PAYOUT_ADMIN, MSG_NOT_SIGNED_IN, PAYOUT_AUTHORITY,
};

use crate::application::orchestrator_client::{
    CompleteOrchestratorAuthRequest, OrchestratorClient, StartOrchestratorAuthRequest,
};
use crate::infrastructure::orchestrator_client::HttpOrchestratorClient;

/// Which Payout Administrator the connected device is: its fingerprint and account xpub must
/// both match one roster entry (which in turn matches params.toml).
/// Oldest Bitcoin app version the payout flows support, and whose screens the checklist lists.
pub const MIN_PAYOUT_APP: (u64, u64, u64) = (2, 4, 6);

/// Which set of review screens the device shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerScreens {
    /// Bitcoin app 2.4.x: no spending-path screens at registration, no "External amounts" /
    /// "You spend" rows, and the fee pass shows the change as a self-transfer.
    V2_4,
    /// Bitcoin app 2.5 and later.
    V2_5,
}

/// The Bitcoin app open on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerApp {
    pub name: String,
    pub version: String,
    parsed: (u64, u64, u64),
}

impl LedgerApp {
    pub fn screens(&self) -> LedgerScreens {
        if self.parsed < (2, 5, 0) {
            LedgerScreens::V2_4
        } else {
            LedgerScreens::V2_5
        }
    }

    /// Set when the app is newer than the versions whose screens were checked (2.4.6, 2.5.1).
    pub fn screens_note(&self) -> Option<String> {
        (self.parsed >= (2, 6, 0)).then(|| {
            format!(
                "These screens were checked on Bitcoin app 2.5.1; your Ledger runs {}. If any \
                 screen differs, reject on the device.",
                self.version
            )
        })
    }
}

pub(super) fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split(['.', '-']).map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// Checks the open app: the network's Bitcoin app, at least [`MIN_PAYOUT_APP`].
pub(super) fn check_ledger_app(
    dep: &Deployment,
    name: &str,
    version: &str,
) -> Result<LedgerApp, String> {
    let expected = if dep.network == bitcoin::Network::Bitcoin {
        "Bitcoin"
    } else {
        "Bitcoin Test"
    };
    if name != expected {
        return Err(if name == "BOLOS" {
            format!("Open the {expected} app on your Ledger.")
        } else {
            format!("The {name} app is open on your Ledger; open the {expected} app.")
        });
    }
    let (ma, mi, pa) = MIN_PAYOUT_APP;
    let parsed = parse_version(version)
        .filter(|v| *v >= MIN_PAYOUT_APP)
        .ok_or_else(|| {
            format!(
                "Update the {expected} app on your Ledger to {ma}.{mi}.{pa} or later in Ledger \
                 Live (installed: {version})."
            )
        })?;
    Ok(LedgerApp {
        name: name.to_string(),
        version: version.to_string(),
        parsed,
    })
}

/// Reads and checks the app open on the device.
pub(super) async fn ledger_app(
    dep: &Deployment,
    device: Arc<dyn PayoutDevice>,
) -> Result<LedgerApp, String> {
    let (name, version) = blocking(move || device.app()).await?;
    check_ledger_app(dep, &name, &version)
}

/// The checklist is written for the Bitcoin app that was open at sign-in. Before a checklist is
/// shown or anything is signed, the device connected now must still run that same app and
/// version: another Ledger with the same seed may show different screens.
pub async fn check_app_unchanged(ctx: &PayoutContext) -> Result<(), String> {
    let now = ledger_app(&ctx.dep, ctx.device.clone()).await?;
    if now != ctx.app {
        return Err(format!(
            "The Ledger now runs {} {}, not {} {} as when you signed in. Sign in again so the \
             list of screens matches this device.",
            now.name, now.version, ctx.app.name, ctx.app.version
        ));
    }
    Ok(())
}

pub(super) async fn identify(
    dep: &Deployment,
    device: Arc<dyn PayoutDevice>,
) -> Result<AdminSigner, String> {
    let signers = dep.signers().map_err(|e| e.to_string())?.to_vec();
    let account = account_path(dep.network);
    let (fp, xpub) = blocking(move || device.identity(&account)).await?;
    let xpub =
        Xpub::from_str(&xpub).map_err(|e| format!("The device returned an invalid xpub: {e}"))?;
    signers
        .into_iter()
        .find(|s| {
            fingerprint_u32(s.fingerprint) == fp
                && s.account_xpub.public_key == xpub.public_key
                && s.account_xpub.chain_code == xpub.chain_code
        })
        .ok_or_else(|| MSG_NOT_A_PAYOUT_ADMIN.to_string())
}

pub(super) async fn sign_in_message(
    me: &AdminSigner,
    dep: &Deployment,
    device: Arc<dyn PayoutDevice>,
    message: String,
) -> Result<SignatureResult, String> {
    let path = me.key_path(dep.network);
    let fp = fingerprint_u32(me.fingerprint);
    let sig = blocking(move || device.sign_message(&message, &path, fp)).await?;
    // The key that signed must be the params.toml key, or the coordinator refuses anyway.
    let pk = bitcoin::PublicKey::from_str(&sig.public_key_hex).map_err(|e| e.to_string())?;
    if XOnlyPublicKey::from(pk.inner) != me.xonly {
        return Err(
            "The device signed with a key that is not this admin's params.toml key.".into(),
        );
    }
    Ok(sig)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayoutSessionDto {
    pub admin: AdminDto,
    pub threshold: usize,
    /// The Bitcoin app on the Ledger, e.g. "Bitcoin Test 2.4.6".
    pub ledger_app: String,
}

/// Before sign-in: connects the Ledger (app version, identity), fetches the coordinator's
/// challenge and returns what the device will show when it signs it. [`sign_in`] then signs that
/// same challenge.
pub async fn sign_in_review(
    state: &PayoutState,
    base_url: &str,
) -> Result<LedgerReviewDto, String> {
    let dep = state.deployment().await?;
    let me = state.connect().await?;
    let app = state.app().await.ok_or(MSG_NOT_SIGNED_IN)?;
    let challenge = sign_in_challenge(&HttpOrchestratorClient::new(base_url.to_string())).await?;
    let review = sign_in_screens(&dep, &me, &app, &challenge.1);
    state.inner.lock().await.challenge = Some(challenge);
    Ok(review)
}

/// What the device shows when it signs the sign-in message.
pub(super) fn sign_in_screens(
    dep: &Deployment,
    me: &AdminSigner,
    app: &LedgerApp,
    message: &str,
) -> LedgerReviewDto {
    let path = me.key_path(dep.network).to_string();
    let mut m = text("Message", message);
    m.note = Some("split over several pages on the device: Message (1/n), (2/n), …".into());
    LedgerReviewDto {
        steps: vec![LedgerStepDto {
            title: "Sign the sign-in message".into(),
            warning: None,
            details: vec![
                text("Review message", ""),
                text("Path", path.trim_start_matches("m/")),
                m,
                text("Sign message", ""),
            ],
        }],
        note: app.screens_note(),
        lock_warning: None,
        txids: Vec::new(),
    }
}

/// Sign in: the coordinator's challenge is signed (BIP-137) on the Ledger with the admin's
/// params.toml key, then the coordinator's deployment is checked against ours. Signs the
/// challenge [`sign_in_review`] showed, or fetches a new one.
pub async fn sign_in(state: &PayoutState, base_url: &str) -> Result<PayoutSessionDto, String> {
    let dep = state.deployment().await?;
    let me = match (state.me().await, state.app().await) {
        (Some(m), Some(_)) => m,
        _ => state.connect().await?,
    };
    let client = HttpOrchestratorClient::new(base_url.to_string());
    let shown = state.inner.lock().await.challenge.take();
    let (challenge_id, message) = match shown {
        Some(c) => c,
        None => sign_in_challenge(&client).await?,
    };
    let sig = sign_in_message(&me, &dep, state.device(), message).await?;
    let session = client
        .auth_verify(CompleteOrchestratorAuthRequest {
            challenge_id,
            signer_pubkey: sig.public_key_hex,
            signature_hex: sig.signature_hex,
            signature_format: "bitcoin-message".to_string(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let coordinator = PayoutCoordinator::new(base_url, &session.token);
    let cfg = coordinator.config().await.map_err(|e| e.to_string())?;
    check_coordinator_deployment(&dep, &cfg)?;
    state.set_session(coordinator).await;
    let app = state.app().await.ok_or(MSG_NOT_SIGNED_IN)?;
    Ok(PayoutSessionDto {
        admin: AdminDto {
            label: admin_label(&dep, me.index),
        },
        threshold: dep.threshold,
        ledger_app: format!("{} {}", app.name, app.version),
    })
}

impl PayoutState {
    pub fn new(device: Arc<dyn PayoutDevice>, policies: PolicyStore) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(SessionInner::default()),
            policies,
            device,
            coins: CoinCache::default(),
            reservations: AdminWalletReservations::default(),
            used_addresses: UsedAddresses::default(),
        }
    }

    /// Shares coin reservations with the Admin Wallet panel, which uses the same wallet.
    pub fn with_reservations(mut self, reservations: AdminWalletReservations) -> Self {
        self.reservations = reservations;
        self
    }

    pub fn ledger(data_dir: Option<std::path::PathBuf>) -> Self {
        let mut state = Self::new(
            Arc::new(LedgerPayoutDevice),
            PolicyStore::new(data_dir.clone()),
        );
        state.used_addresses = UsedAddresses::new(data_dir);
        state
    }

    pub fn device(&self) -> Arc<dyn PayoutDevice> {
        self.device.clone()
    }

    /// Checks the Bitcoin app on the connected Ledger and identifies the Ledger as one of the
    /// admins.
    pub async fn connect(&self) -> Result<AdminSigner, String> {
        let dep = self.deployment().await?;
        let app = ledger_app(&dep, self.device.clone()).await?;
        let me = identify(&dep, self.device.clone()).await?;
        let mut g = self.inner.lock().await;
        if g.me.as_ref().is_some_and(|m| m.index != me.index) {
            g.coordinator = None;
            g.challenge = None;
        }
        g.me = Some(me.clone());
        g.app = Some(app);
        Ok(me)
    }

    pub async fn app(&self) -> Option<LedgerApp> {
        self.inner.lock().await.app.clone()
    }

    pub async fn me(&self) -> Option<AdminSigner> {
        self.inner.lock().await.me.clone()
    }

    pub async fn set_session(&self, coordinator: PayoutCoordinator) {
        self.inner.lock().await.coordinator = Some(Arc::new(coordinator));
    }

    pub async fn sign_out(&self) {
        let mut g = self.inner.lock().await;
        g.coordinator = None;
        g.me = None;
        g.app = None;
        g.challenge = None;
    }

    pub async fn ctx(&self) -> Result<PayoutContext, String> {
        let g = self.inner.lock().await;
        match (&g.deployment, &g.me, &g.app, &g.coordinator) {
            (Some(dep), Some(me), Some(app), Some(c)) => Ok(PayoutContext {
                dep: dep.clone(),
                me: me.clone(),
                app: app.clone(),
                coordinator: c.clone(),
                device: self.device.clone(),
                coins: self.coins.clone(),
                reservations: self.reservations.clone(),
                used_addresses: self.used_addresses.clone(),
            }),
            _ => Err(MSG_NOT_SIGNED_IN.into()),
        }
    }
}

async fn sign_in_challenge(client: &HttpOrchestratorClient) -> Result<(String, String), String> {
    let c = client
        .auth_challenge(StartOrchestratorAuthRequest {
            authority: PAYOUT_AUTHORITY.to_string(),
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok((c.challenge_id, c.challenge_message))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;

    #[test]
    fn app_version_and_name_are_checked() {
        let (_fx, dep, ..) = setup();
        assert_eq!(
            check_ledger_app(&dep, "Bitcoin Test", "2.4.6")
                .unwrap()
                .screens(),
            LedgerScreens::V2_4
        );
        assert_eq!(app_2_5(&dep).screens(), LedgerScreens::V2_5);
        assert!(app_2_5(&dep).screens_note().is_none());
        assert!(check_ledger_app(&dep, "Bitcoin Test", "2.6.0")
            .unwrap()
            .screens_note()
            .is_some());
        let old = check_ledger_app(&dep, "Bitcoin Test", "2.4.5").unwrap_err();
        assert!(
            old.contains("2.4.6 or later") && old.contains("2.4.5"),
            "{old}"
        );
        assert!(check_ledger_app(&dep, "Bitcoin", "2.5.1")
            .unwrap_err()
            .contains("open the Bitcoin Test app"));
        assert!(check_ledger_app(&dep, "BOLOS", "1.0.0")
            .unwrap_err()
            .contains("Open the Bitcoin Test app"));
    }

    #[tokio::test]
    async fn another_app_version_than_at_sign_in_stops_before_the_checklist() {
        let (_fx, dep, me, _) = setup();
        let ctx = |device: &'static str| {
            let mut ctx = signed_in(&dep, &me, PayoutCoordinator::new("http://127.0.0.1:1", "t"));
            ctx.device = Arc::new(TestDevice::app_only(device));
            ctx
        };

        assert_eq!(check_app_unchanged(&ctx("2.5.1")).await, Ok(()));
        // A backup Ledger with the same seed and an older app shows other screens.
        let e = check_app_unchanged(&ctx("2.4.6")).await.unwrap_err();
        assert!(e.contains("2.4.6") && e.contains("Sign in again"), "{e}");
    }

    #[test]
    fn sign_in_screens_show_path_and_message() {
        let (_fx, dep, me, _) = setup();
        let r = sign_in_screens(
            &dep,
            &me,
            &app_2_5(&dep),
            "Strata Session\nRole: payout_admin",
        );
        let d = &r.steps[0].details;
        assert_eq!(d[1].value, "86'/1'/73'/0/0");
        assert_eq!(d[2].value, "Strata Session\nRole: payout_admin");
    }
}
