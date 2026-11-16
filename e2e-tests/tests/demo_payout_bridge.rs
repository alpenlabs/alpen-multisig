//! Manual walkthrough bridge (not a CI test): serves the desktop app's Block Payouts IPC over HTTP
//! so the real React UI can run in a plain browser against the real application layer.
//!
//! Runs only when `PAYOUT_DEMO_DIR` is set and holds the `demo.json` of `demo_payout_stack`. Each
//! `POST /invoke {cmd, args}` calls the same `application::payouts` function as the Tauri command
//! of that name (`commands/payouts.rs`), with the Ledger (`LedgerPayoutDevice`, Speculos when
//! `LEDGER_SPECULOS_URL` is set). `get_node_config` returns the demo's settings; `write_clipboard`
//! is kept and readable at `GET /clipboard`. Other commands answer an error. Stops on `stop`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use desktop_app::application::payouts::{self, LedgerPayoutDevice, PayoutState, PlanRequest};
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
use desktop_app::infrastructure::payout_policy_store::PolicyStore;
use serde_json::{json, Value};

struct Bridge {
    demo: Value,
    state: PayoutState,
    clipboard: Mutex<String>,
    log: Mutex<Vec<String>>,
}

impl Bridge {
    fn node(&self) -> HttpBitcoinRpcClient {
        HttpBitcoinRpcClient::new(
            self.demo["btcRpcUrl"].as_str().unwrap(),
            self.demo["btcRpcUser"].as_str().unwrap(),
            self.demo["btcRpcPass"].as_str().unwrap(),
        )
    }
}

fn s(args: &Value, k: &str) -> Result<String, String> {
    args[k]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing argument {k}"))
}

fn ok<T: serde::Serialize>(r: Result<T, String>) -> Result<Value, String> {
    r.map(|v| serde_json::to_value(v).unwrap())
}

async fn dispatch(b: &Bridge, cmd: &str, args: &Value) -> Result<Value, String> {
    let node = b.node();
    let st = &b.state;
    match cmd {
        "get_node_config" => Ok(json!({
            "mode": "local",
            "customBtcRpcUrl": b.demo["btcRpcUrl"],
            "customBtcRpcUser": b.demo["btcRpcUser"],
            "customBtcRpcPass": "••••",
            "customOrchestratorUrl": b.demo["orchestratorUrl"],
            "payoutParamsPath": b.demo["paramsPath"],
            "payoutRosterPath": b.demo["rosterPath"],
        })),
        "write_clipboard" => {
            *b.clipboard.lock().unwrap() = s(args, "text")?;
            Ok(Value::Null)
        }
        "payout_load_deployment" => {
            let dep = payouts::load_deployment(
                b.demo["paramsPath"].as_str().unwrap(),
                b.demo["rosterPath"].as_str(),
            )?;
            Ok(serde_json::to_value(st.load(dep).await).unwrap())
        }
        "payout_admin_wallet" => {
            let ctx = st.ctx().await?;
            ok(payouts::admin_wallet(&ctx, &node).await)
        }
        "payout_show_receive_address" => {
            let ctx = st.ctx().await?;
            let index = args["index"].as_u64().ok_or("missing argument index")? as u32;
            ok(payouts::show_receive_address(&ctx, index).await)
        }
        "payout_wallet_tip" => {
            use desktop_app::infrastructure::payout_node::PayoutNode;
            ok(node.tip().await)
        }
        "payout_wallet_fee_rate" => {
            use desktop_app::infrastructure::payout_node::PayoutNode;
            ok(node.fee_rate().await)
        }
        "payout_wallet_prepare" => {
            let input: payouts::WalletIntent =
                serde_json::from_value(args["input"].clone()).map_err(|e| e.to_string())?;
            ok(payouts::wallet_prepare(&st.ctx().await?, &input, &node).await)
        }
        "payout_wallet_execute" => {
            let input: payouts::WalletIntent =
                serde_json::from_value(args["input"].clone()).map_err(|e| e.to_string())?;
            ok(
                payouts::wallet_execute(&st.ctx().await?, &input, &s(args, "expectedTxid")?, &node)
                    .await,
            )
        }
        "payout_sign_in_review" => ok(payouts::sign_in_review(st, &s(args, "baseUrl")?).await),
        "payout_sign_in" => ok(payouts::sign_in(st, &s(args, "baseUrl")?).await),
        "payout_sign_out" => {
            st.sign_out().await;
            Ok(Value::Null)
        }
        "payout_overview" => {
            let ctx = st.ctx().await?;
            ok(payouts::overview_for(&ctx, &node).await)
        }
        "payout_verify_reports" => {
            let ctx = st.ctx().await?;
            let pending = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
            let (in_pending, _, _) = payouts::pending_spends(&ctx.dep, &pending);
            ok(payouts::verify_reports(&ctx.dep, &s(args, "text")?, &node, &in_pending).await)
        }
        "payout_plan" => {
            let ctx = st.ctx().await?;
            let req: PlanRequest =
                serde_json::from_value(args["request"].clone()).map_err(|e| e.to_string())?;
            ok(payouts::plan_for(&ctx, &req, &node, false)
                .await
                .map(|(p, _, _)| p))
        }
        "payout_confirm" => {
            let ctx = st.ctx().await?;
            let req: PlanRequest = serde_json::from_value(args["input"]["request"].clone())
                .map_err(|e| e.to_string())?;
            let fee = args["input"]["expectedTotalFeeSat"]
                .as_u64()
                .ok_or("missing fee")?;
            let txids: Vec<String> = serde_json::from_value(args["input"]["expectedTxids"].clone())
                .map_err(|_| "missing expectedTxids")?;
            ok(payouts::confirm_reviewed(&ctx, &req, fee, Some(&txids), &st.policies, &node).await)
        }
        "payout_review_sign" => {
            let ctx = st.ctx().await?;
            ok(payouts::review_sign(&ctx, &s(args, "txid")?, &st.policies, &node).await)
        }
        "payout_review_confirm" => {
            let ctx = st.ctx().await?;
            let req: PlanRequest =
                serde_json::from_value(args["request"].clone()).map_err(|e| e.to_string())?;
            ok(payouts::review_confirm(&ctx, &req, &st.policies, &node).await)
        }
        "payout_sign" => {
            let ctx = st.ctx().await?;
            payouts::check_app_unchanged(&ctx).await?;
            ok(payouts::sign_pending(
                &ctx.dep,
                &ctx.me,
                &s(args, "txid")?,
                ctx.device.clone(),
                &st.policies,
                &ctx.coordinator,
                &node,
            )
            .await)
        }
        "payout_paste_signatures" => {
            let ctx = st.ctx().await?;
            ok(payouts::paste_signatures(
                &ctx.dep,
                &s(args, "txid")?,
                &s(args, "text")?,
                &ctx.coordinator,
                &node,
            )
            .await)
        }
        "payout_import" => {
            let ctx = st.ctx().await?;
            ok(payouts::import(&ctx.dep, &s(args, "text")?, &ctx.coordinator, &node).await)
        }
        "payout_signatures_text" => {
            let ctx = st.ctx().await?;
            let p = ctx
                .coordinator
                .get_pending(&s(args, "txid")?)
                .await
                .map_err(|e| e.to_string())?;
            ok(payouts::signatures_text(&ctx.dep, &p.psbt))
        }
        "payout_retry_broadcast" => {
            let ctx = st.ctx().await?;
            ok(
                payouts::retry_broadcast(&ctx.dep, &s(args, "txid")?, &ctx.coordinator, &node)
                    .await,
            )
        }
        "payout_rebroadcast" => {
            let ctx = st.ctx().await?;
            let past = ctx.coordinator.past().await.map_err(|e| e.to_string())?;
            let txid = s(args, "txid")?;
            let p = past
                .iter()
                .find(|p| p.txid == txid)
                .ok_or("That transaction is not in Past.")?;
            ok(payouts::rebroadcast(&ctx.dep, &txid, &p.psbt, &p.raw_tx, &node).await)
        }
        other => Err(format!("{other}: not available in the walkthrough bridge")),
    }
}

async fn invoke(State(b): State<Arc<Bridge>>, Json(body): Json<Value>) -> Json<Value> {
    let cmd = body["cmd"].as_str().unwrap_or_default().to_string();
    let args = body.get("args").cloned().unwrap_or(Value::Null);
    let r = dispatch(&b, &cmd, &args).await;
    b.log
        .lock()
        .unwrap()
        .push(format!("{cmd}: {}", if r.is_ok() { "ok" } else { "err" }));
    eprintln!(
        "{cmd} -> {}",
        r.as_ref().err().map(String::as_str).unwrap_or("ok")
    );
    Json(match r {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "err": e }),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn payout_demo_bridge() {
    let Ok(dir) = std::env::var("PAYOUT_DEMO_DIR") else {
        eprintln!("skip: PAYOUT_DEMO_DIR not set");
        return;
    };
    let dir = PathBuf::from(dir);
    let demo: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("demo.json")).unwrap()).unwrap();
    let bridge = Arc::new(Bridge {
        demo,
        state: PayoutState::new(
            Arc::new(LedgerPayoutDevice),
            PolicyStore::new(Some(dir.clone())),
        ),
        clipboard: Mutex::new(String::new()),
        log: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/invoke", post(invoke))
        .route(
            "/clipboard",
            get(|State(b): State<Arc<Bridge>>| async move { b.clipboard.lock().unwrap().clone() }),
        )
        .with_state(bridge);
    let addr: SocketAddr = "127.0.0.1:47100".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    eprintln!("bridge on http://{addr}");
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    while !dir.join("stop").exists() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    server.abort();
}
