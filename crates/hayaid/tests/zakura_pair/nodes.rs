//! The two processes of the pair: one hayaid and one zakurad on Regtest, on loopback.
//!
//! Both nodes have the same network: the zcashd Regtest genesis block, Overwinter to NU5 at
//! height 1, the activation heights of [`ACTIVATIONS`], the lockbox disbursement and the
//! funding streams of the [`Setup`], no proof of work (`disable_pow` of Zakura, the
//! Regtest waiver of hayai), and no seeder.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hayai_rpc::cookie::COOKIE_FILE;
use serde_json::{json, Value};

use crate::rpc;

/// The activation heights after NU5: NU6, NU6.1, NU6.2, NU6.3.
pub const ACTIVATIONS: [u32; 4] = [50, 100, 150, 200];

/// The NU7 activation height of the pair. 0: NU7 is not set. Scenario nu7 sets it before it
/// makes its pair.
pub static NU7: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn nu7_height() -> Option<u32> {
    match NU7.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        height => Some(height),
    }
}

/// The Regtest pay-to-script-hash address of the redeem script `OP_TRUE`: each coinbase of
/// both nodes pays to it, so the harness spends a coin without a key.
pub const MINER_ADDRESS: &str = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";

/// The funding streams of a pair that has them: from the NU6 height to the NU6.3 height,
/// 12 % of the block subsidy to the deferred pool and 8 % to an address.
const STREAM_HEIGHTS: std::ops::Range<u32> = ACTIVATIONS[0]..ACTIVATIONS[3];
/// The addresses of the funding stream, in turn: one for each address period of 6 blocks.
const STREAM_ADDRESSES: [&str; 2] = [
    "t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu",
    "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v",
];

pub struct Ports {
    pub hayai_p2p: u16,
    pub hayai_rpc: u16,
    pub hayai_metrics: u16,
    pub zakura_p2p: u16,
    pub zakura_rpc: u16,
    pub zakura_metrics: u16,
    pub zakura_v2: u16,
}

impl Ports {
    pub fn from_base(base: u16) -> Self {
        Self {
            hayai_p2p: base,
            hayai_rpc: base + 1,
            hayai_metrics: base + 2,
            zakura_p2p: base + 20,
            zakura_rpc: base + 21,
            zakura_metrics: base + 22,
            zakura_v2: base + 23,
        }
    }
}

fn local(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// One child process with its PID file. The harness stops only this PID.
pub struct Proc {
    name: &'static str,
    child: Child,
    pid_file: PathBuf,
}

impl Proc {
    fn spawn(name: &'static str, mut command: Command, dir: &Path) -> Result<Self, String> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{name}.log")))
            .map_err(|e| format!("{name} log: {e}"))?;
        let child = command
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("{name}: start: {e}"))?;
        let pid_file = dir.join(format!("{name}.pid"));
        fs::write(&pid_file, child.id().to_string()).map_err(|e| e.to_string())?;
        Ok(Self {
            name,
            child,
            pid_file,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exited(&mut self) -> Result<bool, String> {
        match self.child.try_wait() {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(format!("{}: wait: {e}", self.name)),
        }
    }

    /// Waits until the child ends by itself. Returns whether its exit status is 0. After
    /// `seconds` the function returns an error, and the drop of the value ends the child.
    pub fn wait_exit(mut self, seconds: u64) -> Result<bool, String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    fs::remove_file(&self.pid_file).map_err(|e| e.to_string())?;
                    return Ok(status.success());
                }
                Ok(None) if Instant::now() > deadline => {
                    return Err(format!("{} runs {seconds} s after `stop`", self.name));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("{}: wait: {e}", self.name)),
            }
        }
    }

    /// Sends `signal` to the PID of this child and waits for its end. After 90 s the
    /// harness ends the child with SIGKILL and returns an error.
    pub fn stop(mut self, signal: &str) -> Result<(), String> {
        let pid = self.pid().to_string();
        let status = Command::new("kill")
            .args([format!("-{signal}"), pid.clone()])
            .status()
            .map_err(|e| format!("kill: {e}"))?;
        if !status.success() {
            return Err(format!("{}: kill -{signal} {pid} failed", self.name));
        }
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut result = Ok(());
        while !self.exited()? {
            if Instant::now() > deadline {
                self.child.kill().map_err(|e| e.to_string())?;
                result = Err(format!("{} did not stop in 90 s after {signal}", self.name));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        fs::remove_file(&self.pid_file).map_err(|e| e.to_string())?;
        // A second stop has no process: the drop must not signal a PID again.
        std::mem::forget(self);
        result
    }
}

impl Drop for Proc {
    /// A harness that ends on an error leaves no process.
    fn drop(&mut self) {
        if let Ok(false) = self.exited() {
            if let Err(e) = self.child.kill() {
                eprintln!("{}: kill at drop: {e}", self.name);
            }
            if let Err(e) = self.child.wait() {
                eprintln!("{}: wait at drop: {e}", self.name);
            }
        }
        if self.pid_file.exists() {
            if let Err(e) = fs::remove_file(&self.pid_file) {
                eprintln!("{}: pid file: {e}", self.name);
            }
        }
    }
}

/// How the pair is set up.
#[derive(Clone)]
pub struct Setup {
    pub hayaid_bin: PathBuf,
    pub zakurad_bin: PathBuf,
    pub dir: PathBuf,
    pub ports: Ports,
    /// The amount in zatoshis of the lockbox disbursement that both nodes expect in the
    /// NU6.1 activation block. `None`: no disbursement in the configurations. The Zakura
    /// node then refuses each block at that height, and hayaid does not start.
    pub disbursement: Option<u64>,
    /// Both nodes have the funding streams of [`STREAM_HEIGHTS`].
    pub funding_streams: bool,
    /// The log level of hayaid.
    pub hayai_log: String,
}

impl Clone for Ports {
    fn clone(&self) -> Self {
        Self::from_base(self.hayai_p2p)
    }
}

pub struct Pair {
    pub setup: Setup,
    pub hayaid: Option<Proc>,
    pub zakurad: Option<Proc>,
}

impl Pair {
    pub fn new(setup: Setup) -> Result<Self, String> {
        fs::create_dir_all(&setup.dir).map_err(|e| e.to_string())?;
        let pair = Self {
            setup,
            hayaid: None,
            zakurad: None,
        };
        pair.write_configs()?;
        // Both nodes have the cookie authentication, and one client code serves both.
        // hayaid writes its cookie file to its data directory, zakurad to its `cookie_dir`.
        for (addr, cookie_dir) in [
            (pair.hayai_rpc(), "hayai-data"),
            (pair.zakura_rpc(), "zakura-cookie"),
        ] {
            rpc::set_cookie(addr, pair.setup.dir.join(cookie_dir).join(COOKIE_FILE));
        }
        Ok(pair)
    }

    pub fn hayai_rpc(&self) -> SocketAddr {
        local(self.setup.ports.hayai_rpc)
    }

    pub fn zakura_rpc(&self) -> SocketAddr {
        local(self.setup.ports.zakura_rpc)
    }

    pub fn hayai_metrics(&self) -> SocketAddr {
        local(self.setup.ports.hayai_metrics)
    }

    pub fn zakura_metrics(&self) -> SocketAddr {
        local(self.setup.ports.zakura_metrics)
    }

    fn write_configs(&self) -> Result<(), String> {
        let dir = self.setup.dir.display();
        let p = &self.setup.ports;
        let [nu6, nu6_1, nu6_2, nu6_3] = ACTIVATIONS;
        let (nu7_hayai, nu7_zakura) = match nu7_height() {
            Some(height) => (format!(", nu7 = {height}"), format!("NU7 = {height}\n")),
            None => (String::new(), String::new()),
        };
        // The heights from 50 to 199 are the address periods 8 to 33.
        let addresses: Vec<String> = (0..26)
            .map(|period| format!("\"{}\"", STREAM_ADDRESSES[period % 2]))
            .collect();
        let addresses = addresses.join(", ");
        let (start, end) = (STREAM_HEIGHTS.start, STREAM_HEIGHTS.end);
        let (disbursement_hayai, disbursement_zakura) = match self.setup.disbursement {
            Some(amount) => (
                format!("lockbox_disbursements = [{{ address = \"{MINER_ADDRESS}\", amount = {amount} }}]\n"),
                format!("[[network.testnet_parameters.lockbox_disbursements]]\naddress = \"{MINER_ADDRESS}\"\namount = {amount}\n"),
            ),
            None => (String::new(), String::new()),
        };
        let streams = |table: &str| match self.setup.funding_streams {
            true => format!(
                "[[{table}]]\nheight_range = {{ start = {start}, end = {end} }}\n\
                 [[{table}.recipients]]\nreceiver = \"Deferred\"\nnumerator = 12\n\
                 [[{table}.recipients]]\nreceiver = \"MajorGrants\"\nnumerator = 8\n\
                 addresses = [{addresses}]\n"
            ),
            false => String::new(),
        };
        let streams_hayai = streams("regtest.funding_streams");
        let streams_zakura = streams("network.testnet_parameters.funding_streams");
        let hayai = format!(
            r#"[network]
network = "regtest"
listen_addr = "127.0.0.1:{hp2p}"
peers = ["127.0.0.1:{zp2p}"]
compact_relay = false
seeders = []

[state]
data_dir = "{dir}/hayai-data"

[rpc]
listen_addr = "127.0.0.1:{hrpc}"

[metrics]
listen_addr = "127.0.0.1:{hmet}"

[trace]
dir = "{dir}/hayai-trace"
node = "hayai"

[mining]
miner_address = "{MINER_ADDRESS}"
regtest_produce = true

[log]
level = "{log}"

[regtest]
activation_heights = {{ nu6 = {nu6}, nu6_1 = {nu6_1}, nu6_2 = {nu6_2}, nu6_3 = {nu6_3}{nu7_hayai} }}
{disbursement_hayai}
{streams_hayai}"#,
            log = self.setup.hayai_log,
            hp2p = p.hayai_p2p,
            zp2p = p.zakura_p2p,
            hrpc = p.hayai_rpc,
            hmet = p.hayai_metrics,
        );
        fs::write(self.setup.dir.join("hayai.toml"), &hayai).map_err(|e| e.to_string())?;
        // The same node without a P2P address and without a peer, for the blocks that it
        // mines alone.
        let alone = hayai
            .replace(
                &format!("listen_addr = \"127.0.0.1:{}\"\n", p.hayai_p2p),
                "",
            )
            .replace(
                &format!("peers = [\"127.0.0.1:{}\"]", p.zakura_p2p),
                "peers = []",
            );
        fs::write(self.setup.dir.join("hayai-alone.toml"), alone).map_err(|e| e.to_string())?;
        // No seeder and no bootstrap peer: the peer lists are empty, the peer cache is
        // off, and the legacy stack starts no Zakura (iroh) endpoint. On Regtest Zakura
        // keeps only loopback addresses of its initial peers
        // (`zakura-network/src/config.rs`, `initial_peers`).
        let zakura = format!(
            r#"[network]
network = "Regtest"
listen_addr = "127.0.0.1:{zp2p}"
p2p_stack = "legacy"
cache_dir = false
identity_dir = "{dir}/zakura-identity"
initial_mainnet_peers = []
initial_testnet_peers = []
max_connections_per_ip = 10

[network.testnet_parameters.activation_heights]
NU5 = 1
NU6 = {nu6}
"NU6.1" = {nu6_1}
"NU6.2" = {nu6_2}
"NU6.3" = {nu6_3}
{nu7_zakura}
{disbursement_zakura}
{streams_zakura}
[network.zakura]
bootstrap_peers = []
listen_addr = "127.0.0.1:{zv2}"
trace_dir = "{dir}/zakura-trace"

[state]
cache_dir = "{dir}/zakura-state"

[rpc]
listen_addr = "127.0.0.1:{zrpc}"
enable_cookie_auth = true
cookie_dir = "{dir}/zakura-cookie"

[metrics]
endpoint_addr = "127.0.0.1:{zmet}"

[mining]
miner_address = "{MINER_ADDRESS}"
"#,
            zp2p = p.zakura_p2p,
            zv2 = p.zakura_v2,
            zrpc = p.zakura_rpc,
            zmet = p.zakura_metrics,
        );
        fs::write(self.setup.dir.join("zakurad.toml"), zakura).map_err(|e| e.to_string())
    }

    fn wait_rpc(&mut self, hayai: bool, seconds: u64) -> Result<(), String> {
        let addr = if hayai {
            self.hayai_rpc()
        } else {
            self.zakura_rpc()
        };
        let name = if hayai { "hayaid" } else { "zakurad" };
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let Err(_) = rpc::call(addr, "getblockcount", json!([])) else {
                return Ok(());
            };
            let proc = if hayai {
                self.hayaid.as_mut()
            } else {
                self.zakurad.as_mut()
            };
            if let Some(true) = proc.map(Proc::exited).transpose()? {
                return Err(format!(
                    "{name} stopped at its start; see {}/{name}.log",
                    self.setup.dir.display()
                ));
            }
            if Instant::now() > deadline {
                return Err(format!("{name} RPC did not answer in {seconds} s"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn start_zakurad(&mut self) -> Result<(), String> {
        let mut command = Command::new(&self.setup.zakurad_bin);
        command
            .arg("--config")
            .arg(self.setup.dir.join("zakurad.toml"))
            .arg("start");
        self.zakurad = Some(Proc::spawn("zakurad", command, &self.setup.dir)?);
        self.wait_rpc(false, 120)
    }

    pub fn start_hayaid(&mut self) -> Result<(), String> {
        self.start_hayaid_with("hayai.toml")
    }

    /// Starts hayaid on its data directory without a connection to zakurad.
    pub fn start_hayaid_alone(&mut self) -> Result<(), String> {
        self.start_hayaid_with("hayai-alone.toml")
    }

    fn start_hayaid_with(&mut self, config: &str) -> Result<(), String> {
        let mut command = Command::new(&self.setup.hayaid_bin);
        command
            .arg("start")
            .arg("-c")
            .arg(self.setup.dir.join(config));
        self.hayaid = Some(Proc::spawn("hayaid", command, &self.setup.dir)?);
        self.wait_rpc(true, 300)
    }

    pub fn start_both(&mut self) -> Result<(), String> {
        self.start_zakurad()?;
        self.start_hayaid()?;
        self.wait_connected(60)
    }

    pub fn stop_hayaid(&mut self, signal: &str) -> Result<(), String> {
        match self.hayaid.take() {
            Some(proc) => proc.stop(signal),
            None => Err("hayaid does not run".into()),
        }
    }

    pub fn stop_zakurad(&mut self) -> Result<(), String> {
        match self.zakurad.take() {
            Some(proc) => proc.stop("INT"),
            None => Err("zakurad does not run".into()),
        }
    }

    pub fn stop_all(&mut self) -> Result<(), String> {
        let a = self.hayaid.take().map_or(Ok(()), |p| p.stop("INT"));
        let b = self.zakurad.take().map_or(Ok(()), |p| p.stop("INT"));
        a.and(b)
    }

    /// The number of peers of each node: hayaid from `/metrics`, zakurad from `getpeerinfo`.
    pub fn peer_counts(&self) -> Result<(u64, u64), String> {
        let metrics = rpc::get(self.hayai_metrics(), "/metrics")?;
        let hayai = metrics
            .lines()
            .find_map(|l| l.strip_prefix("hayai_peers "))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .ok_or("no hayai_peers metric")? as u64;
        let zakura = rpc::call(self.zakura_rpc(), "getpeerinfo", json!([]))?
            .as_array()
            .map_or(0, Vec::len) as u64;
        Ok((hayai, zakura))
    }

    pub fn wait_connected(&self, seconds: u64) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if let Ok((1, 1)) = self.peer_counts() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "the nodes are not connected after {seconds} s: {:?}",
                    self.peer_counts()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Checks that each socket of both processes has a loopback address at both ends.
    /// Returns the lines of `ss` that it read.
    pub fn check_loopback_only(&self) -> Result<Vec<String>, String> {
        let output = Command::new("ss")
            .args(["-H", "-tunap"])
            .output()
            .map_err(|e| format!("ss: {e}"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = Vec::new();
        for proc in [&self.hayaid, &self.zakurad].into_iter().flatten() {
            let tag = format!("pid={},", proc.pid());
            for line in text.lines().filter(|l| l.contains(&tag)) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                // netid, state, recv-q, send-q, local, peer.
                for address in [fields.get(4), fields.get(5)].into_iter().flatten() {
                    let loopback = address.starts_with("127.0.0.1:")
                        || address.starts_with("[::1]:")
                        || *address == "0.0.0.0:*"
                        || *address == "*:*";
                    if !loopback {
                        return Err(format!(
                            "{} has a socket outside loopback: {line}",
                            proc.name
                        ));
                    }
                }
                lines.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
        if lines.is_empty() {
            return Err("ss shows no socket of the pair".into());
        }
        Ok(lines)
    }
}

/// `getbestblockhash` of a node.
pub fn best(addr: SocketAddr) -> Result<String, String> {
    match rpc::call(addr, "getbestblockhash", json!([]))? {
        Value::String(hash) => Ok(hash),
        other => Err(format!("getbestblockhash: {other}")),
    }
}

pub fn height(addr: SocketAddr) -> Result<u64, String> {
    rpc::call(addr, "getblockcount", json!([]))?
        .as_u64()
        .ok_or_else(|| "getblockcount: no number".into())
}

/// Waits until `addr` has the tip `hash`. Returns the time that it took.
pub fn wait_tip(addr: SocketAddr, hash: &str, seconds: u64) -> Result<Duration, String> {
    let start = Instant::now();
    loop {
        let tip = best(addr)?;
        if tip == hash {
            return Ok(start.elapsed());
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err(format!(
                "{addr} is at {tip} (height {}) after {seconds} s, not at {hash}",
                height(addr)?
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `generate n` on a node. Returns the hash of the last block.
pub fn generate(addr: SocketAddr, n: u64) -> Result<String, String> {
    let hashes = rpc::call(addr, "generate", json!([n]))?;
    hashes
        .as_array()
        .and_then(|a| a.last())
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("generate: {hashes}"))
}
