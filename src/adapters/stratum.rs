use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::Mutex,
};

use crate::{
    adapters::p2p::GossipNetwork,
    app::{ExternalMineJob, SharedNode, debug_logging_enabled},
    domain::{STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, StratumMineShare},
};

#[derive(Clone)]
pub struct StratumServer {
    node: SharedNode,
    gossip: GossipNetwork,
    listen_addr: SocketAddr,
    next_job_salt: Arc<AtomicU64>,
}

#[derive(Clone, Debug)]
struct StratumJob {
    mine: ExternalMineJob,
}

impl StratumServer {
    pub async fn start(
        node: SharedNode,
        gossip: GossipNetwork,
        listen_addr: SocketAddr,
    ) -> Result<Self> {
        let listener = TcpListener::bind(listen_addr)
            .await
            .with_context(|| format!("failed to bind Stratum listener on {listen_addr}"))?;
        let local_addr = listener.local_addr()?;
        let server = Self {
            node,
            gossip,
            listen_addr: local_addr,
            next_job_salt: Arc::new(AtomicU64::new(1)),
        };
        tokio::spawn(run_listener(server.clone(), listener));
        Ok(server)
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

async fn run_listener(server: StratumServer, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, remote)) => {
                let server = server.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(server, stream).await {
                        if debug_logging_enabled() {
                            eprintln!("stratum session with {remote} failed: {error:#}");
                        }
                    }
                });
            }
            Err(error) if debug_logging_enabled() => {
                eprintln!("stratum accept failed: {error:#}");
            }
            Err(_) => {}
        }
    }
}

async fn handle_connection(server: StratumServer, stream: TcpStream) -> Result<()> {
    let (read, write) = stream.into_split();
    let mut session = StratumSession {
        server,
        writer: Arc::new(Mutex::new(write)),
        authorized_worker: None,
        jobs: BTreeMap::new(),
        next_job_id: 1,
    };
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line).context("invalid Stratum JSON")?;
        session.handle_request(request).await?;
    }
    Ok(())
}

struct StratumSession {
    server: StratumServer,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    authorized_worker: Option<String>,
    jobs: BTreeMap<String, StratumJob>,
    next_job_id: u64,
}

impl StratumSession {
    async fn handle_request(&mut self, request: Value) -> Result<()> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .context("Stratum request is missing method")?;
        match method {
            "mining.subscribe" => {
                self.send_response(
                    id,
                    json!([
                        [["mining.set_difficulty", "iuna"], ["mining.notify", "iuna"]],
                        STRATUM_EXTRANONCE1_HEX,
                        STRATUM_EXTRANONCE2_SIZE
                    ]),
                )
                .await?;
            }
            "mining.authorize" => {
                let worker = request
                    .get("params")
                    .and_then(Value::as_array)
                    .and_then(|params| params.first())
                    .and_then(Value::as_str)
                    .context("mining.authorize requires worker address")?
                    .to_string();
                self.authorized_worker = Some(worker.clone());
                self.send_response(id, json!(true)).await?;
                self.send_job(&worker, true).await?;
            }
            "mining.submit" => {
                let accepted = self.handle_submit(&request).await;
                match accepted {
                    Ok(true) => self.send_response(id, json!(true)).await?,
                    Ok(false) => {
                        self.send_error(id, 23, "duplicate share or transaction")
                            .await?;
                    }
                    Err(error) => self.send_error(id, 23, &format!("{error:#}")).await?,
                }
            }
            "mining.configure" => {
                self.send_response(id, json!({})).await?;
            }
            "mining.extranonce.subscribe" => {
                self.send_response(id, json!(true)).await?;
            }
            _ => {
                self.send_error(id, 20, &format!("unsupported method {method}"))
                    .await?;
            }
        }
        Ok(())
    }

    async fn send_job(&mut self, worker: &str, clean_jobs: bool) -> Result<()> {
        let job_id = self.next_job_id.to_string();
        self.next_job_id = self.next_job_id.saturating_add(1);
        let salt = self.server.next_job_salt.fetch_add(1, Ordering::Relaxed);
        let mine = self
            .server
            .node
            .lock()
            .await
            .external_mine_job(recipient_from_worker(worker), salt)?;
        let difficulty = stratum_difficulty_for_bits(mine.template.difficulty_bits);
        self.send_notification("mining.set_difficulty", json!([difficulty]))
            .await?;
        self.send_notification(
            "mining.notify",
            json!([
                job_id,
                mine.template.prev_hash_hex,
                mine.template.coinb1_hex(),
                "",
                [],
                mine.template.version_hex,
                mine.template.nbits_hex,
                mine.template.ntime_hex,
                clean_jobs
            ]),
        )
        .await?;
        self.jobs.insert(job_id, StratumJob { mine });
        Ok(())
    }

    async fn handle_submit(&mut self, request: &Value) -> Result<bool> {
        let params = request
            .get("params")
            .and_then(Value::as_array)
            .context("mining.submit requires params")?;
        let worker = str_param(params, 0, "worker")?;
        let job_id = str_param(params, 1, "job id")?;
        let extranonce2 = hex_array_4(str_param(params, 2, "extranonce2")?)?;
        let ntime = str_param(params, 3, "ntime")?;
        let header_nonce = hex_array_4(str_param(params, 4, "nonce")?)?;
        let authorized = self
            .authorized_worker
            .as_deref()
            .context("worker is not authorized")?;
        if worker != authorized {
            bail!("submitted worker does not match authorized worker");
        }
        let job = self
            .jobs
            .get(job_id)
            .cloned()
            .context("unknown Stratum job")?;
        if ntime != job.mine.template.ntime_hex {
            bail!("submitted ntime does not match job");
        }

        let (result, outbox) = {
            let mut node = self.server.node.lock().await;
            let result = node.submit_external_mine(
                recipient_from_worker(worker),
                job.mine.template.clone(),
                StratumMineShare {
                    extranonce2,
                    header_nonce,
                },
            );
            let outbox = node.drain_outbox();
            (result, outbox)
        };
        match result {
            Ok(_) => {
                self.server.gossip.broadcast(outbox).await?;
                let worker = worker.to_string();
                self.send_job(&worker, false).await?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    async fn send_response(&self, id: Value, result: Value) -> Result<()> {
        self.send(json!({ "id": id, "result": result, "error": null }))
            .await
    }

    async fn send_error(&self, id: Value, code: i64, message: &str) -> Result<()> {
        self.send(json!({ "id": id, "result": null, "error": [code, message, null] }))
            .await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({ "id": null, "method": method, "params": params }))
            .await
    }

    async fn send(&self, value: Value) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(serde_json::to_string(&value)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        Ok(())
    }
}

fn str_param<'a>(params: &'a [Value], index: usize, name: &str) -> Result<&'a str> {
    params
        .get(index)
        .and_then(Value::as_str)
        .with_context(|| format!("mining.submit requires {name}"))
}

fn recipient_from_worker(worker: &str) -> &str {
    worker
        .split_once('.')
        .map_or(worker, |(recipient, _)| recipient)
}

fn hex_array_4(input: &str) -> Result<[u8; 4]> {
    let bytes = decode_hex(input)?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 4 hex bytes, got {len}"))
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        bytes.push((hex_value(pair[0])? << 4) | hex_value(pair[1])?);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn stratum_difficulty_for_bits(bits: u32) -> f64 {
    2_f64.powi(bits as i32 - 16).max(0.000001)
}
