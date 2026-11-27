use axum::{routing::post, Json, Router};
use payout_admin_core::test_support::Fixture;
use serde_json::{json, Value};
use strata_asm_common::AsmSpec;
use strata_asm_params::{AsmParams, SubprotocolInstance};
use strata_asm_spec::StrataAsmSpec;

pub async fn serve(listener: tokio::net::TcpListener, fixture: &Fixture) -> std::io::Result<()> {
    let template = include_str!("../../../scripts/asm-params.example.json").replace(
        "REPLACE_WITH_64_HEX_CHARS_FROM_getblockhash_101",
        &"0".repeat(64),
    );
    let mut params: AsmParams = serde_json::from_str(&template).expect("valid example ASM params");

    for instance in &mut params.subprotocols {
        if let SubprotocolInstance::Bridge(config) = instance {
            config.operators = (0..fixture.operator_keys().len())
                .map(|index| fixture.operator_xonly(index).into())
                .collect();
        }
    }

    let state = json!({"cur_state": StrataAsmSpec.construct_genesis_state(&params)});
    let app = Router::new().route("/", post(move |Json(request): Json<Value>| {
        let response = if request["method"] == "strata_asm_getStatus" {
            json!({"jsonrpc":"2.0","id":request["id"],"result":state})
        } else {
            json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}})
        };

        async move { Json(response) }
    }));

    axum::serve(listener, app).await
}
