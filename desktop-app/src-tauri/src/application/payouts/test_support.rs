use super::*;
use payout_admin_core::test_support::Fixture;
pub(super) fn app_2_5(dep: &Deployment) -> LedgerApp {
    check_ledger_app(dep, "Bitcoin Test", "2.5.1").unwrap()
}
pub(super) fn signed_in(
    dep: &Deployment,
    me: &AdminSigner,
    coordinator: PayoutCoordinator,
) -> PayoutContext {
    PayoutContext {
        app: check_ledger_app(dep, "Bitcoin Test", "2.5.1").unwrap(),
        dep: Arc::new(dep.clone()),
        me: me.clone(),
        coordinator: Arc::new(coordinator),
        device: Arc::new(TestDevice::app_only("2.5.1")),
    }
}
pub(super) fn setup() -> (Fixture, Deployment, AdminSigner, ()) {
    let fx = Fixture::new(3, 2);
    let dep = fx.deployment();
    let me = dep.signers().unwrap()[0].clone();
    (fx, dep, me, ())
}
pub(super) struct TestDevice {
    version: &'static str,
}
impl TestDevice {
    pub(super) fn app_only(version: &'static str) -> Self {
        Self { version }
    }
}
impl PayoutDevice for TestDevice {
    fn app(&self) -> Result<(String, String), String> {
        Ok(("Bitcoin Test".into(), self.version.into()))
    }
    fn identity(&self, _: &DerivationPath) -> Result<(u32, String), String> {
        unreachable!()
    }
    fn sign_message(&self, _: &str, _: &DerivationPath, _: u32) -> Result<SignatureResult, String> {
        unreachable!()
    }
}
