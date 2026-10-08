use super::*;
use std::{
    fs::File,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

pub struct StockClients {
    root: PathBuf,
    logs: PathBuf,
    url: String,
    processes: Vec<Child>,
    before: Option<BalancesResponse>,
    command_number: usize,
}
impl Drop for StockClients {
    fn drop(&mut self) {
        for child in &mut self.processes {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl StockClients {
    pub fn build() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let output = Command::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "server",
                "-p",
                "cli",
                "-p",
                "maker",
            ])
            .current_dir(&root)
            .output()
            .expect("cargo must be installed");
        assert!(
            output.status.success(),
            "release build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let logs = root
            .join("target")
            .join(format!("stock-clients-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&logs).unwrap();
        Self {
            root,
            logs,
            url: String::new(),
            processes: Vec::new(),
            before: None,
            command_number: 0,
        }
    }
    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(
            self.root
                .join("target/release")
                .join(format!("{binary}{}", std::env::consts::EXE_SUFFIX)),
        );
        command.current_dir(&self.root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    }
    fn spawn(&mut self, binary: &str, args: &[&str], name: &str) -> usize {
        let child = self
            .command(binary)
            .args(args)
            .stdout(Stdio::from(
                File::create(self.logs.join(format!("{name}.out.log"))).unwrap(),
            ))
            .stderr(Stdio::from(
                File::create(self.logs.join(format!("{name}.err.log"))).unwrap(),
            ))
            .spawn()
            .expect("test process should start");
        self.processes.push(child);
        self.processes.len() - 1
    }
    pub async fn start_server(&mut self, seed: &str, accept_secs: u64) {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port().to_string();
        drop(probe);
        self.url = format!("http://127.0.0.1:{port}");
        let server = self.spawn(
            "server",
            &[
                "--seed",
                seed,
                "--port",
                &port,
                "--accept-window-secs",
                &accept_secs.to_string(),
            ],
            "server",
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap();
        timeout(Duration::from_secs(8), async {
            loop {
                assert!(
                    self.processes[server].try_wait().unwrap().is_none(),
                    "server exited; logs: {}",
                    self.logs.display()
                );
                if let Ok(response) = http.get(format!("{}/health", self.url)).send().await {
                    if response.status() == 200 {
                        break;
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("server should become ready");
        self.before = Some(self.balances().await);
    }
    async fn cli(&mut self, args: &[&str], expected: &str) {
        self.command_number += 1;
        let name = format!("cli-{}", self.command_number);
        let url = self.url.clone();
        let mut all_args = vec!["--server", url.as_str()];
        all_args.extend_from_slice(args);
        let index = self.spawn("rfq-cli", &all_args, &name);
        let status = timeout(Duration::from_secs(20), async {
            loop {
                if let Some(status) = self.processes[index].try_wait().unwrap() {
                    break status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("CLI command should finish");
        let output = std::fs::read_to_string(self.logs.join(format!("{name}.out.log"))).unwrap();
        assert!(
            status.success(),
            "CLI failed; logs: {}",
            self.logs.display()
        );
        assert!(
            output.contains(expected),
            "expected {expected:?} in CLI output: {output}"
        );
    }
    pub async fn expect_sol_is_listed(&mut self) {
        self.cli(&["assets"], "SOL").await;
    }
    pub async fn expect_request_without_makers_to_expire(&mut self, sol: u64, ttl_secs: u64) {
        self.cli(
            &[
                "--key",
                "keys/taker.json",
                "submit",
                "--asset",
                "SOL",
                "--size",
                &sol.to_string(),
                "--direction",
                "buy",
                "--ttl",
                &ttl_secs.to_string(),
                "--yes",
            ],
            "RFQ expired",
        )
        .await;
    }
    pub async fn start_offline_makers(&mut self, strategies: &[&str], random_seed: u64) {
        for strategy in strategies {
            let key = format!("keys/maker-{strategy}.json");
            let name = format!("maker-{strategy}");
            let url = self.url.clone();
            let index = self.spawn(
                "rfq-maker",
                &[
                    "--server",
                    &url,
                    "--key",
                    &key,
                    "--strategy",
                    strategy,
                    "--offline",
                    "--rng-seed",
                    &random_seed.to_string(),
                ],
                &name,
            );
            timeout(Duration::from_secs(8), async {
                loop {
                    assert!(
                        self.processes[index].try_wait().unwrap().is_none(),
                        "maker {strategy} exited; logs: {}",
                        self.logs.display()
                    );
                    let log =
                        std::fs::read_to_string(self.logs.join(format!("{name}.out.log"))).unwrap();
                    if log.contains("subscribed to /ws/makers") {
                        break;
                    }
                    sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("maker should subscribe");
        }
    }
    pub async fn buy_sol(&mut self, size: u64) {
        self.trade("SOL", size, "buy", false).await;
    }
    pub async fn sell_sol(&mut self, size: u64) {
        self.trade("SOL", size, "sell", false).await;
    }
    pub async fn buy_doge_from_two_way_quote(&mut self, size: u64) {
        self.trade("DOGE", size, "buy", true).await;
    }
    pub async fn sell_doge_from_two_way_quote(&mut self, size: u64) {
        self.trade("DOGE", size, "sell", true).await;
    }
    async fn trade(&mut self, asset: &str, size: u64, side: &str, two_way: bool) {
        let size = size.to_string();
        let mut args = vec![
            "--key",
            "keys/taker.json",
            "submit",
            "--asset",
            asset,
            "--size",
            &size,
            "--ttl",
            "1",
        ];
        if two_way {
            args.extend(["--accept", side]);
        } else {
            args.extend(["--direction", side, "--yes"]);
        }
        self.cli(&args, "RFQ settled").await;
    }
    async fn balances(&self) -> BalancesResponse {
        reqwest::get(format!("{}/v1/balances", self.url))
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    pub async fn expect_balances_command_to_work(&mut self) {
        self.cli(&["balances"], "taker").await;
    }
    pub async fn expect_cash_and_every_asset_conserved(&self) {
        fn totals(balances: &BalancesResponse) -> BTreeMap<String, u128> {
            let mut sums = BTreeMap::new();
            for trader in &balances.traders {
                *sums.entry("USD".into()).or_default() += trader.usd.total as u128;
                for (asset, balance) in &trader.assets {
                    *sums.entry(asset.clone()).or_default() += balance.total as u128;
                }
            }
            sums
        }
        assert_eq!(
            totals(self.before.as_ref().unwrap()),
            totals(&self.balances().await),
            "stock-client trades must conserve every currency"
        );
    }
    pub async fn expect_everyones_funds_released(&self) {
        for trader in self.balances().await.traders {
            assert_eq!(trader.usd.reserved, 0, "cash reservation leaked");
            for (asset, balance) in trader.assets {
                assert_eq!(balance.reserved, 0, "{asset} reservation leaked");
            }
        }
        println!(
            "Stock CLI and all four offline makers passed. Logs: {}",
            self.logs.display()
        );
    }
}
