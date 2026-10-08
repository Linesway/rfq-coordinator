use super::*;
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
};

pub struct ServerProcess {
    child: Child,
    directory: PathBuf,
    pub url: String,
}
impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.directory.join("seed.toml"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}
impl ServerProcess {
    pub async fn start(accept: Option<u64>) -> Self {
        let directory = std::env::temp_dir().join(format!("rfq-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let key = Keypair::from_seed([1; 32]);
        let maker = Keypair::from_seed([2; 32]);
        let seed=format!("[assets.SOL]\nlot_decimals = 2\n[[traders]]\npubkey = \"{}\"\nusd = 1000000\n[traders.assets]\nSOL = 1000\n[[traders]]\npubkey = \"{}\"\nusd = 1000000\n[traders.assets]\nSOL = 1000\n",key.pubkey(),maker.pubkey());
        std::fs::write(directory.join("seed.toml"), seed).unwrap();
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_server"));
        cmd.arg("--seed")
            .arg(directory.join("seed.toml"))
            .arg("--port")
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(seconds) = accept {
            cmd.arg("--accept-window-secs").arg(seconds.to_string());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let child = cmd.spawn().unwrap();
        let mut server = Self {
            child,
            directory,
            url: format!("http://127.0.0.1:{port}"),
        };
        timeout(Duration::from_secs(8), async {
            let http = reqwest::Client::builder()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap();
            loop {
                assert!(
                    server.child.try_wait().unwrap().is_none(),
                    "server exited before health became ready"
                );
                if let Ok(response) = http.get(format!("{}/health", server.url)).send().await {
                    if response.status() == 200 {
                        break;
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("server startup timed out");
        server
    }
}

impl ServerProcess {
    pub fn trader(&self, name: &str) -> LiveTrader {
        let key = match name {
            "Alice" => 1,
            "Bob" => 2,
            _ => panic!("unknown trader: {name}"),
        };
        LiveTrader::connect(&self.url, key)
    }
    pub async fn expect_request_missing(&self, request: uuid::Uuid) {
        let response = reqwest::get(format!("{}/v1/rfqs/{request}", self.url))
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
}
pub fn expect_startup_failure(arguments: &[&str]) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_server"));
    command.args(arguments);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("server binary should run");
    assert!(
        !output.status.success(),
        "invalid arguments should fail: {arguments:?}"
    );
    assert!(
        !output.stderr.is_empty(),
        "failure should explain what went wrong"
    );
}
pub fn missing_seed_path() -> String {
    std::env::temp_dir()
        .join(format!("missing-{}.toml", uuid::Uuid::new_v4()))
        .to_string_lossy()
        .into_owned()
}
