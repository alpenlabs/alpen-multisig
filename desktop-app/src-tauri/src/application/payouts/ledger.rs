use super::{
    address_on_device, admin_label, payout_tx, policy, AdminSigner, Arc, BTreeSet, Deployment,
    DerivationPath, Fingerprint, FromStr, LedgerApp, LedgerScreens, PolicySpec, PolicyStore, Psbt,
    Serialize, SignatureResult,
};
use crate::infrastructure::hw_wallet::ledger::payout as device_api;

/// What the flows need from a signing device. Blocking; called from `spawn_blocking`.
pub trait PayoutDevice: Send + Sync {
    /// The open app's name and version, e.g. `("Bitcoin Test", "2.4.6")`.
    fn app(&self) -> Result<(String, String), String>;
    /// Master fingerprint (`u32::from_le_bytes`) and account xpub at `account`.
    fn identity(&self, account: &DerivationPath) -> Result<(u32, String), String>;
    fn register_policy(&self, p: &PolicySpec, fingerprint: u32) -> Result<[u8; 32], String>;
    /// The registered policy's receive address 0, as the device derives it.
    fn policy_address(&self, p: &PolicySpec, hmac: [u8; 32]) -> Result<String, String>;
    /// Shows the receive address `index` of the single-key policy `p` (the Admin Wallet) on the
    /// device screen, and returns it.
    fn show_address(&self, p: &PolicySpec, index: u32) -> Result<String, String>;
    fn sign_psbt(
        &self,
        psbt: &mut Psbt,
        p: &PolicySpec,
        hmac: Option<[u8; 32]>,
        fingerprint: u32,
    ) -> Result<usize, String>;
    fn sign_message(
        &self,
        message: &str,
        path: &DerivationPath,
        fingerprint: u32,
    ) -> Result<SignatureResult, String>;
}

/// The Ledger (HID, or Speculos with `LEDGER_SPECULOS_URL`).
pub struct LedgerPayoutDevice;

impl PayoutDevice for LedgerPayoutDevice {
    fn app(&self) -> Result<(String, String), String> {
        device_api::app()
    }
    fn identity(&self, account: &DerivationPath) -> Result<(u32, String), String> {
        device_api::identity(account)
    }
    fn register_policy(&self, p: &PolicySpec, fingerprint: u32) -> Result<[u8; 32], String> {
        device_api::register_policy(p, fingerprint)
    }
    fn policy_address(&self, p: &PolicySpec, hmac: [u8; 32]) -> Result<String, String> {
        device_api::policy_address(p, Some(hmac), 0, false)
    }
    fn show_address(&self, p: &PolicySpec, index: u32) -> Result<String, String> {
        device_api::policy_address(p, None, index, true)
    }
    fn sign_psbt(
        &self,
        psbt: &mut Psbt,
        p: &PolicySpec,
        hmac: Option<[u8; 32]>,
        fingerprint: u32,
    ) -> Result<usize, String> {
        device_api::sign_psbt(psbt, p, hmac, fingerprint)
    }
    fn sign_message(
        &self,
        message: &str,
        path: &DerivationPath,
        fingerprint: u32,
    ) -> Result<SignatureResult, String> {
        device_api::sign_message(message, path, fingerprint)
    }
}

pub fn fingerprint_u32(fp: Fingerprint) -> u32 {
    u32::from_le_bytes(fp.to_bytes())
}

pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("device task failed: {e}"))?
}

/// Ensures the covenant's multisig policy is registered on the device, verifying on first
/// registration that the device derives the connector address from it.
pub(super) async fn ensure_policy(
    dep: &Deployment,
    me: &AdminSigner,
    cov_index: usize,
    device: Arc<dyn PayoutDevice>,
    store: &PolicyStore,
) -> Result<(PolicySpec, [u8; 32]), String> {
    let cov = &dep.covenants[cov_index];
    let p = policy::admin_multisig_policy(dep, cov).map_err(|e| e.to_string())?;
    let id = device_api::policy_id(&p)?;
    let fp = fingerprint_u32(me.fingerprint);
    if let Some(h) = store.get(fp, &id) {
        return Ok((p, h));
    }
    let (p2, d2) = (p.clone(), device.clone());
    let hmac = blocking(move || d2.register_policy(&p2, fp)).await?;
    let (p3, d3) = (p.clone(), device);
    let shown = blocking(move || d3.policy_address(&p3, hmac)).await?;
    let expected = policy::connector_address(dep, cov);
    let shown_script = bitcoin::Address::from_str(&shown)
        .map_err(|e| format!("The device returned an invalid address: {e}"))?
        .assume_checked()
        .script_pubkey();
    if shown_script != expected.script_pubkey() {
        return Err(format!(
            "The device derives {shown} from the payout policy, not the claim connector {expected}."
        ));
    }
    store.save(fp, &id, &hmac)?;
    Ok((p, hmac))
}

/// Signs every claim input of `psbt` as `me` (one device approval per covenant involved).
pub(super) async fn sign_connectors(
    dep: &Deployment,
    me: &AdminSigner,
    psbt: &mut Psbt,
    device: Arc<dyn PayoutDevice>,
    store: &PolicyStore,
) -> Result<(), String> {
    let a = payout_tx::normalize(dep, psbt).map_err(|e| e.to_string())?;
    let covs: BTreeSet<usize> = a
        .connector_inputs
        .iter()
        .filter(|c| !c.signed_by.contains(&me.index))
        .map(|c| c.covenant_index)
        .collect();
    let fp = fingerprint_u32(me.fingerprint);
    for cov in covs {
        let (p, hmac) = ensure_policy(dep, me, cov, device.clone(), store).await?;
        *psbt = device_sign(psbt, device.clone(), p, Some(hmac), fp).await?;
    }
    let a = payout_tx::normalize(dep, psbt).map_err(|e| e.to_string())?;
    if !a
        .connector_inputs
        .iter()
        .all(|c| c.signed_by.contains(&me.index))
    {
        return Err("The device did not sign every claim input.".into());
    }
    Ok(())
}

/// Signs the fee inputs with the Admin Wallet policy.
pub(super) async fn sign_fee_inputs(
    dep: &Deployment,
    me: &AdminSigner,
    psbt: &mut Psbt,
    device: Arc<dyn PayoutDevice>,
) -> Result<(), String> {
    let p = policy::admin_wallet_policy(me, dep.network);
    let fp = fingerprint_u32(me.fingerprint);
    *psbt = device_sign(psbt, device, p, None, fp).await?;
    let a = payout_tx::analyze(dep, psbt).map_err(|e| e.to_string())?;
    if !a.fee_inputs.iter().all(|f| f.signed) {
        return Err("The device did not sign every fee input.".into());
    }
    Ok(())
}

/// One line the device shows. `sat` is set for amounts (the screen formats them like the device).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailDto {
    /// The label as the device shows it.
    pub label: String,
    pub value: String,
    pub sat: Option<u64>,
    /// Context only the app can add (who a key belongs to); not on the device.
    pub note: Option<String>,
}

/// One interaction with the device, in the order it happens.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerStepDto {
    pub title: String,
    /// A device warning that is expected at this step, with why it is safe to continue.
    pub warning: Option<String>,
    pub details: Vec<DetailDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerReviewDto {
    pub steps: Vec<LedgerStepDto>,
    /// Set when the Ledger's app is newer than the versions whose screens were checked.
    pub note: Option<String>,
    /// Set when a transaction has so many claims that the Ledger may lock while it works.
    pub lock_warning: Option<String>,
    /// The unsigned ids of the transactions this checklist describes, in order (none for the
    /// sign-in). Confirm signs only if it builds exactly these.
    pub txids: Vec<String>,
}

/// From this many claims in one transaction, a Ledger with the shortest PIN lock (1 minute) can
/// lock while it loads or signs.
pub const LOCK_WARNING_CLAIMS: usize = 15;

/// Warns that the Ledger may lock (go to sleep) while it works on `claims` inputs, and how to
/// prevent it. The device counts the time since the last button press, also while it loads and
/// signs; a lock then ends the signing with an error.
///
/// Times measured on a Nano X with Bitcoin Test 2.4.6 and a 2-of-3 leaf, 10 and 20 claims: loading
/// the claim pass 4 s + 2.0 s per claim, signing it 1.9 s per claim, loading the fee pass
/// 3 s + 1.3 s per claim. Signing runs straight into the fee pass's loading.
pub(super) fn lock_warning(claims: usize, with_fee_pass: bool) -> Option<String> {
    if claims < LOCK_WARNING_CLAIMS {
        return None;
    }
    let n = claims as f64;
    let seconds = if with_fee_pass {
        3.0 + 3.2 * n
    } else {
        4.0 + 2.0 * n
    };
    let minutes = (seconds / 60.0).ceil() as u64;
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    // First line: what to do, at a glance. Second line: how.
    Some(format!(
        "Keep your Ledger awake: expected waiting time {minutes} {unit}\n\
         Solutions: (1) Press the Ledger's buttons every half minute or so. (2) Increase the lock \
         time on the Ledger: Settings → Security → PIN lock."
    ))
}

pub(super) fn text(label: impl Into<String>, value: impl Into<String>) -> DetailDto {
    DetailDto {
        label: label.into(),
        value: value.into(),
        sat: None,
        note: None,
    }
}

pub(super) fn amount(label: impl Into<String>, sat: u64) -> DetailDto {
    DetailDto {
        label: label.into(),
        value: String::new(),
        sat: Some(sat),
        note: None,
    }
}

pub(super) const EXTERNAL_INPUTS_WARNING: &str =
    "First screen: \"Security risk: There are external inputs. Reject if not sure\". Choose \
     \"Continue anyway\". This transaction has claim inputs and fee inputs, and the device signs \
     one kind per pass, so it calls the other kind external. This app has already checked every \
     input.";

/// "@1", "@1 and @2", "@1, @2 and @3": how the device lists the signer slots.
pub(super) fn slot_list(n: usize) -> String {
    let slots: Vec<String> = (1..=n).map(|i| format!("@{i}")).collect();
    match slots.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => slots.join(""),
    }
}

// Per-version labels: account origin and the optional external/net totals, in device order.
// 2.5+ also shows registration spending paths; 2.4 shows a self-transfer for the fee pass.
const SCREEN_LABELS: [(&str, &[&str]); 2] = [
    ("From", &[]),
    ("From account", &["External amounts", "You spend"]),
];
fn screen_labels(app: &LedgerApp) -> (&'static str, &'static [&'static str]) {
    SCREEN_LABELS[usize::from(app.screens() == LedgerScreens::V2_5)]
}
fn signing_step(title: String, mut details: Vec<DetailDto>, fee_sat: u64) -> LedgerStepDto {
    details.push(amount("Fees", fee_sat));
    LedgerStepDto {
        title,
        warning: Some(EXTERNAL_INPUTS_WARNING.into()),
        details,
    }
}

/// The Ledger registration screens of one covenant's payout wallet, in the device's wording.
pub(super) fn registration_step(
    dep: &Deployment,
    me: &AdminSigner,
    app: &LedgerApp,
    cov_index: usize,
) -> Result<LedgerStepDto, String> {
    let cov = &dep.covenants[cov_index];
    let p = policy::admin_multisig_policy(dep, cov).map_err(|e| e.to_string())?;
    let n = p.keys.len() - 1;
    let mut details = vec![text("Account name", p.name.clone())];
    // 2.5 explains the policy's spending paths; 2.4 shows only the policy itself.
    if !screen_labels(app).1.is_empty() {
        details.push(text("Primary spending path", "Main path: spendable by @0"));
        details.push(text(
            "Spending path #1",
            format!("Any {} of {} must sign", dep.threshold, slot_list(n)),
        ));
    }
    details.push(text("Wallet policy", p.template.clone()));
    for (i, key) in p.keys.iter().enumerate() {
        let ours = i > 0 && i - 1 == me.index;
        let mut d = text(
            format!("{} key@{i}", if ours { "Our" } else { "Their" }),
            key.clone(),
        );
        d.note = Some(if i == 0 {
            "bridge aggregate key".to_string()
        } else {
            admin_label(dep, i - 1)
        });
        details.push(d);
    }
    Ok(LedgerStepDto {
        title: "Register the account on your Ledger".into(),
        warning: None,
        details,
    })
}

/// What the device will show while it signs `psbts` as `me`, step by step, in the order
/// [`sign_connectors`] and [`sign_fee_inputs`] ask for it: per transaction, one claim pass per
/// covenant this admin still has to sign for (each preceded by the registration of that
/// covenant's payout wallet when this device has not registered it yet), then the fee pass when
/// `with_fee_pass` (Confirm signs it, Sign on a pending transaction does not). In a claim pass
/// the device owns only that covenant's inputs; every other input is external to it. Labels and
/// amounts follow the screens of the device's Bitcoin app version (2.4.x or 2.5+), as seen on
/// Speculos.
pub(super) fn ledger_review(
    dep: &Deployment,
    me: &AdminSigner,
    app: &LedgerApp,
    psbts: &[Psbt],
    with_fee_pass: bool,
    store: &PolicyStore,
) -> Result<LedgerReviewDto, String> {
    ledger_review_for(dep, me, app, psbts, with_fee_pass, store, None)
}

/// [`ledger_review`], saying whose Admin Wallet receives the change when another admin created
/// the transaction (`change_owner`, checked by [`check_pending_for_signing`]).
pub(super) fn ledger_review_for(
    dep: &Deployment,
    me: &AdminSigner,
    app: &LedgerApp,
    psbts: &[Psbt],
    with_fee_pass: bool,
    store: &PolicyStore,
    change_owner: Option<usize>,
) -> Result<LedgerReviewDto, String> {
    let fp = fingerprint_u32(me.fingerprint);
    let (from, totals) = screen_labels(app);
    // Registered earlier in this same run: the device asks once per payout wallet.
    let mut registered = BTreeSet::new();
    let mut steps = Vec::new();
    let mut txids = Vec::new();
    let mut most_claims = 0;
    for (i, psbt) in psbts.iter().enumerate() {
        let a = payout_tx::analyze(dep, psbt).map_err(|e| e.to_string())?;
        let change = psbt.unsigned_tx.output.first().ok_or("no change output")?;
        let which = if psbts.len() > 1 {
            format!("transaction {} of {}", i + 1, psbts.len())
        } else {
            "the transaction".to_string()
        };
        let fee_input_sat: u64 = a.fee_inputs.iter().map(|f| f.value_sat).sum();
        let claim_input_sat = a.input_total_sat - fee_input_sat;
        // The change belongs to another wallet from the multisig's point of view, so the claim
        // pass lists it as a payment ("Amount" / "To").
        let here = bitcoin::Address::from_script(&change.script_pubkey, dep.network)
            .map(|a| a.to_string())
            .unwrap_or_default();
        // Confirm signs this admin's own transaction. Sign signs one another admin created: its
        // change must go to an admin's Admin Wallet, and the caller has checked whose.
        let whose = match change_owner {
            _ if with_fee_pass => "your change address".to_string(),
            Some(owner) if owner == me.index => {
                "change to your Admin Wallet (address checked by this app)".to_string()
            }
            Some(owner) => format!(
                "change to the Admin Wallet of {} (address checked by this app)",
                admin_label(dep, owner)
            ),
            None => "the change output of the transaction".to_string(),
        };
        let to = || match address_on_device(dep, &change.script_pubkey) {
            Some(device) => {
                let mut d = text("To", device);
                d.note = Some(format!("{here} in this app; {whose}"));
                d
            }
            None => {
                let mut d = text("To", here.clone());
                d.note = Some(whose.clone());
                d
            }
        };
        txids.push(a.txid.to_string());
        let unsigned: Vec<_> = a
            .connector_inputs
            .iter()
            .filter(|c| !c.signed_by.contains(&me.index))
            .collect();
        most_claims = most_claims.max(unsigned.len());
        let covs: BTreeSet<usize> = unsigned.iter().map(|c| c.covenant_index).collect();
        for &cov in &covs {
            let covenant = &dep.covenants[cov];
            let p = policy::admin_multisig_policy(dep, covenant).map_err(|e| e.to_string())?;
            let id = device_api::policy_id(&p)?;
            if store.get(fp, &id).is_none() && registered.insert(cov) {
                steps.push(registration_step(dep, me, app, cov)?);
            }
            // The device signs with one payout wallet per pass: its own inputs are this
            // covenant's claims, and the other covenants' claims count as external like the fee
            // inputs do.
            let own_sat = covenant.connector_tx_out().value.to_sat()
                * a.connector_inputs
                    .iter()
                    .filter(|c| c.covenant_index == cov)
                    .count() as u64;
            let mut claim = vec![
                text(from, p.name.clone()),
                amount("Amount", change.value.to_sat()),
                to(),
            ];
            claim.extend(
                totals
                    .iter()
                    .zip([a.input_total_sat - own_sat, own_sat])
                    .map(|(&label, sat)| amount(label, sat)),
            );
            steps.push(signing_step(
                if covs.len() > 1 {
                    format!("Sign {which}: claim inputs of {}", p.name)
                } else {
                    format!("Sign {which}: claim inputs")
                },
                claim,
                a.fee_sat,
            ));
        }
        if with_fee_pass {
            let details = if totals.is_empty() {
                // 2.4 sees the output as its own: a self-transfer.
                let unit = if dep.network == bitcoin::Network::Bitcoin {
                    "BTC"
                } else {
                    "TEST"
                };
                let mut amt = text("Amount", format!("0 {unit} (self-transfer)"));
                amt.note = Some("the change returns to your Admin Wallet".into());
                vec![amt]
            } else {
                vec![
                    amount(totals[0], claim_input_sat),
                    // Claims can cover the fee, leaving the wallet with a net receipt.
                    match fee_input_sat.checked_sub(change.value.to_sat()) {
                        Some(spent) => amount(totals[1], spent),
                        None => amount("You receive", change.value.to_sat() - fee_input_sat),
                    },
                ]
            };
            steps.push(signing_step(
                format!("Sign {which}: fee inputs"),
                details,
                a.fee_sat,
            ));
        }
    }
    Ok(LedgerReviewDto {
        steps,
        note: app.screens_note(),
        lock_warning: lock_warning(most_claims, with_fee_pass),
        txids,
    })
}

/// Runs the blocking device call on an owned PSBT; validation stays with each signing flow.
pub(super) async fn device_sign(
    psbt: &Psbt,
    device: Arc<dyn PayoutDevice>,
    policy: PolicySpec,
    hmac: Option<[u8; 32]>,
    fingerprint: u32,
) -> Result<Psbt, String> {
    let mut work = psbt.clone();
    blocking(move || {
        device.sign_psbt(&mut work, &policy, hmac, fingerprint)?;
        Ok(work)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;
    use bitcoin::hashes::Hash;

    #[test]
    fn two_covenants_are_two_claim_passes_each_after_its_registration() {
        let fx = Fixture::new(3, 2);
        // The operator set changes at height 200: two covenants, two payout wallets.
        let params =
            fx.params_toml_with_schedule(&[(0, 101, None), (1, 101, None), (2, 200, None)]);
        let dep = Deployment::load(&params, Some(&fx.roster_toml())).unwrap();
        assert_eq!(dep.covenants.len(), 2);
        let me = dep.signers().unwrap()[0].clone();
        let connectors: Vec<ConnectorRef> = (0..2)
            .map(|c| ConnectorRef {
                claim_txid: Txid::from_byte_array([7 + c as u8; 32]),
                covenant_index: c,
            })
            .collect();
        let connector_sat = dep.covenants[0].connector_tx_out().value.to_sat();
        let psbt = payout_tx::build_psbt(
            &dep,
            &connectors,
            &[fee_coin(&fx, 0, 0, 50_000)],
            &change(&fx, 49_000),
        )
        .unwrap();
        let names: Vec<String> = dep
            .covenants
            .iter()
            .map(policy::admin_policy_name)
            .collect();
        let r = ledger_review(
            &dep,
            &me,
            &app_2_5(&dep),
            std::slice::from_ref(&psbt),
            true,
            &PolicyStore::new(None),
        )
        .unwrap();
        let titles: Vec<_> = r.steps.iter().map(|s| s.title.clone()).collect();
        // The order the app asks the device: register A, sign A, register B, sign B, fee pass.
        assert_eq!(
            titles,
            [
                "Register the account on your Ledger".to_string(),
                format!("Sign the transaction: claim inputs of {}", names[0]),
                "Register the account on your Ledger".to_string(),
                format!("Sign the transaction: claim inputs of {}", names[1]),
                "Sign the transaction: fee inputs".to_string(),
            ]
        );
        for (step, name) in [(1, &names[0]), (3, &names[1])] {
            let s = &r.steps[step];
            assert_eq!(&value(s, "From account").value, name);
            // The other covenant's claim is external to this pass, like the fee input.
            assert_eq!(
                value(s, "External amounts").sat,
                Some(50_000 + connector_sat)
            );
            assert_eq!(value(s, "You spend").sat, Some(connector_sat));
        }
        assert_eq!(value(&r.steps[0], "Account name").value, names[0]);
        assert_eq!(value(&r.steps[2], "Account name").value, names[1]);
        assert_eq!(
            value(&r.steps[4], "External amounts").sat,
            Some(2 * connector_sat)
        );
        // Bitcoin app 2.4: the same passes, with its own labels.
        let app = check_ledger_app(&dep, "Bitcoin Test", "2.4.6").unwrap();
        let r = ledger_review(&dep, &me, &app, &[psbt], true, &PolicyStore::new(None)).unwrap();
        assert_eq!(r.steps.len(), 5);
        assert_eq!(&value(&r.steps[3], "From").value, &names[1]);
    }

    #[test]
    fn many_claims_warn_that_the_ledger_may_lock() {
        let (_fx, dep, me, psbt) = setup();
        let app = check_ledger_app(&dep, "Bitcoin Test", "2.4.6").unwrap();
        let r = ledger_review(&dep, &me, &app, &[psbt], true, &PolicyStore::new(None)).unwrap();
        assert!(r.lock_warning.is_none(), "few claims: no warning");
        assert!(lock_warning(LOCK_WARNING_CLAIMS - 1, true).is_none());
        let w = lock_warning(LOCK_WARNING_CLAIMS, true).unwrap();
        assert!(
            w.starts_with("Keep your Ledger awake: expected waiting time 1 minute\n"),
            "{w}"
        );
        assert!(w.contains("buttons every half minute") && w.contains("PIN lock"));
        // 90 claims: signing and the fee pass's loading take about 5 minutes; signing another
        // admin's payout has no fee pass, so the first loading is the longest stretch.
        assert!(lock_warning(90, true).unwrap().contains("time 5 minutes"));
        assert!(lock_warning(90, false).unwrap().contains("time 4 minutes"));
    }

    #[test]
    fn app_2_4_screens() {
        let (_fx, dep, me, psbt) = setup();
        let app = check_ledger_app(&dep, "Bitcoin Test", "2.4.6").unwrap();
        let r = ledger_review(&dep, &me, &app, &[psbt], true, &PolicyStore::new(None)).unwrap();
        let labels = |i: usize| -> Vec<String> {
            r.steps[i].details.iter().map(|d| d.label.clone()).collect()
        };
        // Registration: no spending-path screens.
        assert_eq!(labels(0)[..2], ["Account name", "Wallet policy"]);
        assert_eq!(labels(1), ["From", "Amount", "To", "Fees"]);
        assert_eq!(labels(2), ["Amount", "Fees"]);
        assert_eq!(r.steps[2].details[0].value, "0 TEST (self-transfer)");
    }

    #[test]
    fn unregistered_wallet_is_reviewed_before_signing() {
        let (_fx, dep, me, psbt) = setup();
        let r = ledger_review(
            &dep,
            &me,
            &app_2_5(&dep),
            &[psbt],
            true,
            &PolicyStore::new(None),
        )
        .unwrap();
        let titles: Vec<_> = r.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles.len(), 3, "{titles:?}");
        assert_eq!(titles[0], "Register the account on your Ledger");
        assert_eq!(titles[1], "Sign the transaction: claim inputs");
        assert_eq!(titles[2], "Sign the transaction: fee inputs");

        // Registration, in the device's wording, with every key as the device receives it.
        let reg = &r.steps[0];
        let name = value(reg, "Account name").value.clone();
        assert_eq!(name, policy::admin_policy_name(&dep.covenants[0]));
        assert_eq!(
            value(reg, "Spending path #1").value,
            "Any 2 of @1, @2 and @3 must sign"
        );
        let p = policy::admin_multisig_policy(&dep, &dep.covenants[0]).unwrap();
        assert_eq!(value(reg, "Wallet policy").value, p.template);
        for (i, key) in p.keys.iter().enumerate() {
            let who = if i == 1 { "Our" } else { "Their" };
            assert_eq!(value(reg, &format!("{who} key@{i}")).value, *key);
        }
        assert!(p.keys[1].contains("tpub"));

        // Claim pass: change as a device-form payment, the claim and fee input totals, the fee.
        let claim = &r.steps[1];
        assert_eq!(value(claim, "From account").value, name);
        assert_eq!(value(claim, "Amount").sat, Some(49_000));
        assert!(value(claim, "To").value.starts_with("tb1p"));
        assert_eq!(value(claim, "External amounts").sat, Some(50_000));
        assert_eq!(value(claim, "You spend").sat, Some(330));
        assert_eq!(value(claim, "Fees").sat, Some(50_000 + 330 - 49_000));
        assert!(claim
            .warning
            .as_deref()
            .unwrap()
            .contains("Continue anyway"));

        // Fee pass: the roles swap; the change is the wallet's own so nothing is paid out.
        let fee = &r.steps[2];
        assert_eq!(value(fee, "External amounts").sat, Some(330));
        assert_eq!(value(fee, "You spend").sat, Some(1_000));
        assert_eq!(value(fee, "Fees").sat, Some(1_330));
    }

    #[test]
    fn fee_pass_says_you_receive_when_the_claims_pay_the_fee() {
        let (fx, dep, me, _) = setup();
        let connectors = [7u8, 8].map(|n| ConnectorRef {
            claim_txid: Txid::from_byte_array([n; 32]),
            covenant_index: 0,
        });
        // 40_000 in, 40_331 back as change; the two claims (660) cover the fee, as in the demo.
        let psbt = payout_tx::build_psbt(
            &dep,
            &connectors,
            &[fee_coin(&fx, 0, 0, 40_000)],
            &change(&fx, 40_331),
        )
        .unwrap();
        let r = ledger_review(
            &dep,
            &me,
            &app_2_5(&dep),
            &[psbt],
            true,
            &PolicyStore::new(None),
        )
        .unwrap();
        let fee = r.steps.last().unwrap();
        assert!(fee.details.iter().all(|d| d.label != "You spend"));
        assert_eq!(value(fee, "External amounts").sat, Some(660));
        assert_eq!(value(fee, "You receive").sat, Some(331));
        assert_eq!(value(fee, "Fees").sat, Some(329));
    }

    #[test]
    fn registered_wallet_is_not_reviewed_again() {
        let (_fx, dep, me, psbt) = setup();
        let store = PolicyStore::new(None);
        let p = policy::admin_multisig_policy(&dep, &dep.covenants[0]).unwrap();
        let id = device_api::policy_id(&p).unwrap();
        store
            .save(fingerprint_u32(me.fingerprint), &id, &[1; 32])
            .unwrap();
        let r = ledger_review(
            &dep,
            &me,
            &app_2_5(&dep),
            std::slice::from_ref(&psbt),
            false,
            &store,
        )
        .unwrap();
        let titles: Vec<_> = r.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Sign the transaction: claim inputs"]);
        // Sign: another admin created it, so the change is theirs.
        let change_note = |r: &LedgerReviewDto| {
            let to = r.steps[0].details.iter().find(|d| d.label == "To").unwrap();
            to.note.clone().unwrap()
        };
        assert!(change_note(&r).ends_with("the change output of the transaction"));
        // Confirm: this admin's own transaction.
        let r = ledger_review(&dep, &me, &app_2_5(&dep), &[psbt], true, &store).unwrap();
        assert!(change_note(&r).ends_with("your change address"));
    }
}
