//! End to end tests for madara.
#![cfg(test)]

mod rpc;
mod storage_proof;

use anyhow::bail;
use rstest::rstest;
use starknet_providers::Provider;
use starknet_providers::{jsonrpc::HttpTransport, JsonRpcClient, Url};
use std::ops::Range;
use std::sync::Mutex;
use std::{
    collections::HashMap,
    env,
    future::Future,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    str::FromStr,
    time::Duration,
};
use tempfile::TempDir;

async fn wait_for_cond<F: Future<Output = Result<(), anyhow::Error>>>(
    mut cond: impl FnMut() -> F,
    duration: Duration,
    max_attempts: u32,
) {
    let mut attempt = 0;
    loop {
        let Err(err) = cond().await else {
            break;
        };

        attempt += 1;
        if attempt >= max_attempts {
            panic!("No answer from the node after {attempt} attempts: {:#}", err)
        }

        tokio::time::sleep(duration).await;
    }
}

pub struct MadaraCmd {
    process: Option<Child>,
    ready: bool,
    json_rpc: Option<JsonRpcClient<HttpTransport>>,
    rpc_url: Url,
    tempdir: TempDir,
    _port: MadaraPortNum,
}

impl MadaraCmd {
    pub fn wait_with_output(mut self) -> Output {
        self.process.take().unwrap().wait_with_output().unwrap()
    }

    pub fn json_rpc(&mut self) -> &JsonRpcClient<HttpTransport> {
        self.json_rpc.get_or_insert_with(|| JsonRpcClient::new(HttpTransport::new(self.rpc_url.clone())))
    }

    pub fn db_dir(&self) -> &Path {
        self.tempdir.path()
    }

    pub async fn wait_for_ready(&mut self) -> &mut Self {
        let endpoint = self.rpc_url.join("/health").unwrap();
        wait_for_cond(
            || async {
                let res = reqwest::get(endpoint.clone()).await?;
                res.error_for_status()?;
                anyhow::Ok(())
            },
            Duration::from_secs(2),
            10,
        )
        .await;
        self.ready = true;
        self
    }

    // TODO: replace this with `subscribeNewHeads`
    pub async fn wait_for_sync_to(&mut self, block_n: u64) -> &mut Self {
        let rpc = self.json_rpc();
        wait_for_cond(
            || async {
                match rpc.block_hash_and_number().await {
                    Ok(got) => {
                        tracing::info!("Received block number {} out of {block_n}", got.block_number);

                        if got.block_number < block_n {
                            bail!("got block_n {}, expected {block_n}", got.block_number);
                        }
                        anyhow::Ok(())
                    }
                    Err(err) => bail!(err),
                }
            },
            Duration::from_secs(2),
            100,
        )
        .await;
        self
    }
}

impl Drop for MadaraCmd {
    fn drop(&mut self) {
        let Some(mut child) = self.process.take() else { return };
        let kill = || {
            let mut kill = Command::new("kill").args(["-s", "TERM", &child.id().to_string()]).spawn()?;
            kill.wait()?;
            anyhow::Ok(())
        };
        if let Err(_err) = kill() {
            child.kill().unwrap()
        }
        child.wait().unwrap();
    }
}

// this really should use unix sockets, sad

const PORT_RANGE: Range<u16> = 19944..20000;

struct AvailablePorts<I: Iterator<Item = u16>> {
    to_reuse: Vec<u16>,
    next: I,
}

lazy_static::lazy_static! {
    static ref AVAILABLE_PORTS: Mutex<AvailablePorts<Range<u16>>> = Mutex::new(AvailablePorts { to_reuse: vec![], next: PORT_RANGE });
}

pub struct MadaraPortNum(pub u16);
impl Drop for MadaraPortNum {
    fn drop(&mut self) {
        let mut guard = AVAILABLE_PORTS.lock().expect("poisoned lock");
        guard.to_reuse.push(self.0);
    }
}

pub fn get_port() -> MadaraPortNum {
    let mut guard = AVAILABLE_PORTS.lock().expect("poisoned lock");
    if let Some(el) = guard.to_reuse.pop() {
        return MadaraPortNum(el);
    }
    let port = guard.next.next().expect("no more port to use");
    MadaraPortNum(port)
}

pub struct MadaraCmdBuilder {
    args: Vec<String>,
    env: HashMap<String, String>,
    tempdir: TempDir,
    port: MadaraPortNum,
}

impl Default for MadaraCmdBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MadaraCmdBuilder {
    pub fn new() -> Self {
        Self {
            args: Default::default(),
            env: Default::default(),
            tempdir: TempDir::with_prefix("madara-test").unwrap(),
            port: get_port(),
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Self {
        self.env = env.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        self
    }

    pub fn run(self) -> MadaraCmd {
        let target_bin = env::var("COVERAGE_BIN").expect("env COVERAGE_BIN to be set by script");
        let target_bin = PathBuf::from_str(&target_bin).expect("COVERAGE_BIN to be a path");
        if !target_bin.exists() {
            panic!("No binary to run: {:?}", target_bin)
        }

        // This is an optional argument to sync faster from the FGW if gateway_key is set
        let gateway_key_arg = env::var("GATEWAY_KEY").ok().map(|gateway_key| ["--gateway-key".into(), gateway_key]);

        let process = Command::new(target_bin)
            .envs(self.env)
            .args(
                self.args
                    .into_iter()
                    .chain([
                        "--base-path".into(),
                        format!("{}", self.tempdir.as_ref().display()),
                        "--rpc-port".into(),
                        format!("{}", self.port.0),
                    ])
                    .chain(gateway_key_arg.into_iter().flatten()),
            )
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        MadaraCmd {
            process: Some(process),
            ready: false,
            json_rpc: None,
            rpc_url: Url::parse(&format!("http://127.0.0.1:{}/", self.port.0)).unwrap(),
            tempdir: self.tempdir,
            _port: self.port,
        }
    }
}

#[rstest]
fn madara_help_shows() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let output = MadaraCmdBuilder::new().args(["--help"]).run().wait_with_output();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Madara: High performance Starknet sequencer/full-node"), "stdout: {stdout}");
}

#[rstest]
#[tokio::test]
async fn madara_can_sync_a_few_blocks() {
    use starknet_core::types::BlockHashAndNumber;
    use starknet_types_core::felt::Felt;

    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let cmd_builder = MadaraCmdBuilder::new().args([
        "--full",
        "--network",
        "sepolia",
        "--no-sync-polling",
        "--n-blocks-to-sync",
        "20",
        "--no-l1-sync",
        "--gas-price",
        "0",
    ]);

    let mut node = cmd_builder.run();
    node.wait_for_ready().await;
    node.wait_for_sync_to(19).await;

    assert_eq!(
        node.json_rpc().block_hash_and_number().await.unwrap(),
        BlockHashAndNumber {
            // https://sepolia.voyager.online/block/0x4177d1ba942a4ab94f86a476c06f0f9e02363ad410cdf177c54064788c9bcb5
            block_hash: Felt::from_hex_unchecked("0x4177d1ba942a4ab94f86a476c06f0f9e02363ad410cdf177c54064788c9bcb5"),
            block_number: 19
        }
    );
}
