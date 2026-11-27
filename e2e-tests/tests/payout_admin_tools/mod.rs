//! Isolated Docker services and a real coordinator subprocess; no hardware access.
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, Command},
    time::Duration,
};

pub fn docker(args: &[&str]) {
    let output = Command::new("docker")
        .env("DOCKER_CONTEXT", "colima")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub struct Stack;

impl Stack {
    pub fn start() -> Self {
        docker(&["network", "create", "payout-rt-net"]);
        let stack = Self;
        docker(&[
            "run",
            "-d",
            "--name",
            "payout-rt-bitcoin",
            "--network",
            "payout-rt-net",
            "--network-alias",
            "bitcoin",
            "-p",
            "127.0.0.1:18400:18443",
            "bitcoin/bitcoin:30",
            "-regtest",
            "-server",
            "-rpcbind=0.0.0.0",
            "-rpcallowip=0.0.0.0/0",
            "-rpcuser=user",
            "-rpcpassword=password",
            "-fallbackfee=0.00002",
        ]);
        docker(&[
            "run",
            "-d",
            "--name",
            "payout-rt-postgres",
            "--network",
            "payout-rt-net",
            "-p",
            "127.0.0.1:18402:5432",
            "-e",
            "POSTGRES_PASSWORD=payout-test",
            "postgres:16",
        ]);

        stack
    }

    pub fn electrs(&self) {
        docker(&[
            "run",
            "-d",
            "--name",
            "payout-rt-electrs",
            "--network",
            "payout-rt-net",
            "-p",
            "127.0.0.1:18401:60401",
            "payout-rt-electrs",
        ]);
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for name in [
            "payout-rt-electrs",
            "payout-rt-bitcoin",
            "payout-rt-postgres",
        ] {
            let _ = Command::new("docker")
                .env("DOCKER_CONTEXT", "colima")
                .args(["rm", "-fv", name])
                .output();
        }
        let _ = Command::new("docker")
            .env("DOCKER_CONTEXT", "colima")
            .args(["network", "rm", "payout-rt-net"])
            .output();
    }
}

pub fn electrum(method: &str, params: Value) -> Result<Value, String> {
    let mut stream = TcpStream::connect(
        std::env::var("PAYOUT_TEST_ELECTRUM").unwrap_or_else(|_| "127.0.0.1:18401".into()),
    )
    .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let response_json: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;

    if !response_json["error"].is_null() {
        return Err(response_json.to_string());
    }

    Ok(response_json["result"].clone())
}

pub struct Coordinator(pub Child);

impl Coordinator {
    pub async fn start(signers: &std::path::Path, asm_url: &str, postgres: bool) -> Self {
        let mut command = Command::new(
            std::env::var("PAYOUT_RT_SERVER").expect("set PAYOUT_RT_SERVER to built server binary"),
        );
        command
            .env("SERVER_HOST", "127.0.0.1")
            .env("SERVER_PORT", "18403")
            .env("PAYOUT_ADMIN_PUBLIC_URL", "http://127.0.0.1:18403")
            .env("PAYOUT_SIGNERS_PATH", signers)
            .env("PAYOUT_ELECTRUM_URL", "tcp://127.0.0.1:18401")
            .env("STRATA_ADMIN_STATE_RPC_URL", asm_url)
            .env("BITCOIN_RPC_URL", "http://127.0.0.1:1")
            .env("BITCOIN_NETWORK", "regtest")
            .env("RUST_LOG", "error")
            .current_dir(std::env::temp_dir());

        if postgres {
            command.env(
                "DATABASE_URL",
                "postgres://postgres:payout-test@127.0.0.1:18402/postgres",
            );
        } else {
            command.env_remove("DATABASE_URL");
        }

        let mut server = Self(command.spawn().unwrap());
        for _ in 0..100 {
            assert!(server.0.try_wait().unwrap().is_none(), "coordinator exited");

            if reqwest::get("http://127.0.0.1:18403/api/v1/health")
                .await
                .is_ok()
            {
                return server;
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("coordinator did not start");
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub async fn api(path: &str, token: &str, body: Option<Value>) -> Value {
    let client = reqwest::Client::new();
    let url = format!(
        "{}{path}",
        std::env::var("PAYOUT_TEST_API").unwrap_or_else(|_| "http://127.0.0.1:18403".into())
    );
    let request = if let Some(body) = body {
        client.post(url).json(&body)
    } else {
        client.get(url)
    };
    let response = request.bearer_auth(token).send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status}: {text}");

    serde_json::from_str(&text).unwrap()
}

/// Core is used only to seed/mine the test chain, never by payout verification or coordination.
pub struct Node;

impl Node {
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let response = reqwest::Client::new()
            .post(
                std::env::var("PAYOUT_TEST_RPC")
                    .unwrap_or_else(|_| "http://127.0.0.1:18400".into()),
            )
            .basic_auth("user", Some("password"))
            .json(&json!({"jsonrpc":"1.0", "id":1, "method":method, "params":params}))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let value: Value = response.json().await.map_err(|error| error.to_string())?;

        if !value["error"].is_null() {
            return Err(value.to_string());
        }

        Ok(value["result"].clone())
    }
}
