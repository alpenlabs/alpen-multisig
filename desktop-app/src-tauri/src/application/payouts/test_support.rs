use super::*;
use bitcoin::hashes::Hash;
pub(super) use payout_admin_core::test_support::{change, fee_coin, sign_as, sign_fee, Fixture};

pub(super) fn app_2_5(dep: &Deployment) -> LedgerApp {
    check_ledger_app(dep, "Bitcoin Test", "2.5.1").unwrap()
}

/// A node that knows the given outputs, and which of them are spent.
#[derive(Default)]
pub(super) struct MockNode {
    pub(super) outputs: HashMap<OutPoint, bitcoin::TxOut>,
    pub(super) spent: HashSet<OutPoint>,
    pub(super) scanned: Vec<crate::infrastructure::payout_node::ScannedCoin>,
    pub(super) mempool: HashSet<Txid>,
    pub(super) mempool_spent: HashSet<OutPoint>,
    pub(super) reject: bool,
    pub(super) accepted_on_error: bool,
    pub(super) descendants: Option<(u64, u64)>,
    pub(super) reserve_on_unspent: Option<AdminWalletReservations>,
    pub(super) unspent_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl PayoutNode for MockNode {
    async fn tx(
        &self,
        txid: &Txid,
    ) -> Result<Option<crate::infrastructure::payout_node::NodeTx>, String> {
        let n = self
            .outputs
            .keys()
            .filter(|o| o.txid == *txid)
            .map(|o| o.vout + 1)
            .max();
        Ok(n.map(|n| {
            let filler = bitcoin::TxOut {
                value: bitcoin::Amount::ZERO,
                script_pubkey: ScriptBuf::new(),
            };
            crate::infrastructure::payout_node::NodeTx {
                tx: bitcoin::Transaction {
                    version: bitcoin::transaction::Version(2),
                    lock_time: bitcoin::absolute::LockTime::ZERO,
                    input: vec![],
                    output: (0..n)
                        .map(|v| {
                            self.outputs
                                .get(&OutPoint::new(*txid, v))
                                .cloned()
                                .unwrap_or_else(|| filler.clone())
                        })
                        .collect(),
                },
                block_height: Some(150),
                block_time: None,
                confirmations: 6,
            }
        }))
    }
    async fn unspent(&self, o: &OutPoint, include_mempool: bool) -> Result<bool, String> {
        if let Some(shared) = &self.reserve_on_unspent {
            if self
                .unspent_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 1
            {
                shared.set_in_flight([*o]);
            }
        }
        Ok(self.outputs.contains_key(o)
            && !self.spent.contains(o)
            && !(include_mempool && self.mempool_spent.contains(o)))
    }
    async fn broadcast(&self, tx: &bitcoin::Transaction) -> Result<Txid, String> {
        if self.reject {
            return Err("node refused replacement".into());
        }
        Ok(tx.compute_txid())
    }
    async fn tip(&self) -> Result<u64, String> {
        Ok(160)
    }
    async fn version(&self) -> Result<u64, String> {
        Ok(300_000)
    }
    async fn scan(
        &self,
        _descriptors: &[String],
    ) -> Result<Vec<crate::infrastructure::payout_node::ScannedCoin>, String> {
        Ok(self.scanned.clone())
    }
    async fn mempool_package(
        &self,
        _: &Txid,
    ) -> Result<Option<crate::infrastructure::payout_node::MempoolPackage>, String> {
        Ok(self.descendants.map(|(fee_sat, vsize)| {
            crate::infrastructure::payout_node::MempoolPackage { fee_sat, vsize }
        }))
    }
    async fn in_mempool(&self, txid: &Txid) -> Result<bool, String> {
        Ok(self.accepted_on_error || self.mempool.contains(txid))
    }
}

/// The node view in which every input of `psbt` is exactly what the PSBT states.
pub(super) fn node_for(psbt: &Psbt) -> MockNode {
    MockNode {
        outputs: psbt
            .unsigned_tx
            .input
            .iter()
            .zip(&psbt.inputs)
            .map(|(i, p)| (i.previous_output, p.witness_utxo.clone().unwrap()))
            .collect(),
        spent: HashSet::new(),
        ..Default::default()
    }
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
pub(super) async fn send_fixture(
    max: bool,
) -> (PayoutContext, MockNode, WalletSendRequest, Arc<TestDevice>) {
    let (fx, dep, me, _) = setup();
    let mut ctx = signed_in(&dep, &me, coordinator_answering("[]".into()).await);
    let device = Arc::new(TestDevice {
        version: "2.5.1",
        signing: true,
        calls: Default::default(),
    });
    ctx.device = device.clone();
    let coin = fee_coin(&fx, 0, 0, 100_000);
    let mut node = MockNode::default();
    node.outputs.insert(coin.outpoint, coin.txout.clone());
    node.scanned
        .push(crate::infrastructure::payout_node::ScannedCoin {
            outpoint: coin.outpoint,
            value_sat: 100_000,
            script_pubkey: coin.txout.script_pubkey,
            desc: "tr([00000000/86h/1h/73h/0/0]key)".into(),
            height: 100,
        });
    let address =
        bitcoin::Address::from_script(&change_or_receive_script(&me, 0, 1).unwrap(), dep.network)
            .unwrap()
            .to_string();
    let mut req = WalletSendRequest {
        address,
        amount_sat: Some(20_000),
        max,
        fee_rate: "2".into(),
        expected_txid: None,
    };
    req.expected_txid = Some(
        wallet_prepare(&ctx, &WalletIntent::Send(req.clone()), &node)
            .await
            .unwrap()
            .txid,
    );
    (ctx, node, req, device)
}

/// A coordinator that answers every request with `body` (JSON).
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
        reservations: Default::default(),
        sends: WalletSendStore::default(),
        used_addresses: UsedAddresses::default(),
    }
}

pub(super) fn stored_pending(psbt: &Psbt, expires_at: i64) -> CoordinatorPending {
    CoordinatorPending {
        txid: psbt.unsigned_tx.compute_txid().to_string(),
        psbt: psbt.to_string(),
        created_by: String::new(),
        created_at: 0,
        expires_at,
        broadcast_error: None,
        signatures_received: 0,
        signatures_needed: 0,
        duplicate_outpoints: Vec::new(),
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
