use super::*;
use bitcoin::hashes::Hash;
pub(super) use payout_admin_core::test_support::{change, fee_coin, Fixture};
pub(super) fn app_2_5(dep: &Deployment) -> LedgerApp {
    check_ledger_app(dep, "Bitcoin Test", "2.5.1").unwrap()
}
pub(super) struct TestDevice {
    version: &'static str,
    signing: bool,
    pub(super) calls: std::sync::atomic::AtomicUsize,
}
impl PayoutDevice for TestDevice {
    fn app(&self) -> Result<(String, String), String> {
        Ok(("Bitcoin Test".into(), self.version.into()))
    }
    fn identity(&self, _: &DerivationPath) -> Result<(u32, String), String> {
        unreachable!()
    }
    fn register_policy(&self, _: &PolicySpec, _: u32) -> Result<[u8; 32], String> {
        unreachable!()
    }
    fn policy_address(&self, _: &PolicySpec, _: [u8; 32]) -> Result<String, String> {
        unreachable!()
    }
    fn show_address(&self, _: &PolicySpec, _: u32) -> Result<String, String> {
        unreachable!()
    }
    fn sign_message(&self, _: &str, _: &DerivationPath, _: u32) -> Result<SignatureResult, String> {
        unreachable!()
    }
    fn sign_psbt(
        &self,
        psbt: &mut Psbt,
        _: &PolicySpec,
        _: Option<[u8; 32]>,
        _: u32,
    ) -> Result<usize, String> {
        assert!(self.signing, "unexpected signing call");
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let fx = Fixture::new(3, 2);
        let coin = fee_coin(&fx, 0, 0, 100_000);
        payout_admin_core::test_support::sign_fee(&fx, psbt, &coin, 0);
        Ok(1)
    }
}
pub(super) async fn coordinator_answering(body: String) -> PayoutCoordinator {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let answer = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    PayoutCoordinator::new(&url, "t")
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
        coins: Default::default(),
    }
}
pub(super) fn setup() -> (Fixture, Deployment, AdminSigner, Psbt) {
    let fx = Fixture::new(3, 2);
    let dep = Deployment::load(&fx.params_toml(), Some(&fx.roster_toml())).unwrap();
    let me = dep.signers().unwrap()[0].clone();
    let connector = ConnectorRef {
        claim_txid: Txid::from_byte_array([7; 32]),
        covenant_index: 0,
    };
    let psbt = payout_tx::build_psbt(
        &dep,
        &[connector],
        &[fee_coin(&fx, 0, 0, 50_000)],
        &change(&fx, 49_000),
    )
    .unwrap();
    (fx, dep, me, psbt)
}
pub(super) fn value<'a>(step: &'a LedgerStepDto, label: &str) -> &'a DetailDto {
    step.details
        .iter()
        .find(|d| d.label.starts_with(label))
        .unwrap_or_else(|| panic!("no detail {label} in {step:?}"))
}
impl TestDevice {
    pub(super) fn app_only(version: &'static str) -> Self {
        Self {
            version,
            signing: false,
            calls: Default::default(),
        }
    }
}
