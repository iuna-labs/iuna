use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

use crate::{
    adapters::p2p::GossipNetwork,
    app::{ExternalMineJob, SharedNode, debug_logging_enabled},
    domain::{STRATUM_EXTRANONCE1_HEX, STRATUM_EXTRANONCE2_SIZE, StratumMineShare},
};

const STRATUM_MAX_LINE_BYTES: usize = 16 * 1024;
const STRATUM_MAX_JOBS_PER_SESSION: usize = 128;
const STRATUM_MAX_SESSIONS: usize = 64;
const STRATUM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(feature = "fuzzing")]
pub fn fuzz_parse_stratum_request(line: &str) -> Result<Value> {
    if line.len() > STRATUM_MAX_LINE_BYTES {
        bail!("Stratum request exceeds {STRATUM_MAX_LINE_BYTES} byte limit");
    }
    serde_json::from_str(line).context("invalid Stratum JSON")
}

#[derive(Clone)]
pub struct StratumServer {
    node: SharedNode,
    gossip: GossipNetwork,
    listen_addr: SocketAddr,
    next_job_salt: Arc<AtomicU64>,
    session_limiter: StratumSessionLimiter,
}

#[derive(Clone, Debug)]
struct StratumJob {
    mine: ExternalMineJob,
}

#[derive(Clone)]
struct StratumSessionLimiter {
    permits: Arc<Semaphore>,
}

impl StratumSessionLimiter {
    fn new(max_sessions: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_sessions)),
        }
    }

    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }
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
            session_limiter: StratumSessionLimiter::new(STRATUM_MAX_SESSIONS),
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
                let Some(permit) = server.session_limiter.try_acquire() else {
                    if debug_logging_enabled() {
                        eprintln!("stratum session with {remote} rejected: session limit reached");
                    }
                    continue;
                };
                let server = server.clone();
                tokio::spawn(async move {
                    let _permit = permit;
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
    let mut lines = StratumLineReader::new(read);
    loop {
        let Some(line) = timeout(STRATUM_IDLE_TIMEOUT, lines.read_line())
            .await
            .context("Stratum session idle timeout")??
        else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line).context("invalid Stratum JSON")?;
        session.handle_request(request).await?;
    }
    Ok(())
}

struct StratumLineReader<R> {
    reader: BufReader<R>,
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> StratumLineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            pending: Vec::new(),
        }
    }

    async fn read_line(&mut self) -> Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                bail!("client closed before completing a Stratum request");
            }

            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if self.pending.len() + newline > STRATUM_MAX_LINE_BYTES {
                    bail!("Stratum request exceeds {STRATUM_MAX_LINE_BYTES} byte limit");
                }
                self.pending.extend_from_slice(&available[..newline]);
                self.reader.consume(newline + 1);
                if self.pending.ends_with(b"\r") {
                    self.pending.pop();
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes)
                    .context("Stratum request is not valid UTF-8")
                    .map(Some);
            }

            if self.pending.len() + available.len() > STRATUM_MAX_LINE_BYTES {
                bail!("Stratum request exceeds {STRATUM_MAX_LINE_BYTES} byte limit");
            }
            let consumed = available.len();
            self.pending.extend_from_slice(available);
            self.reader.consume(consumed);
        }
    }
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
        insert_bounded_job(&mut self.jobs, job_id, StratumJob { mine });
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

fn insert_bounded_job(jobs: &mut BTreeMap<String, StratumJob>, job_id: String, job: StratumJob) {
    jobs.insert(job_id, job);
    while jobs.len() > STRATUM_MAX_JOBS_PER_SESSION {
        let Some(oldest) = oldest_job_id(jobs) else {
            break;
        };
        jobs.remove(&oldest);
    }
}

fn oldest_job_id(jobs: &BTreeMap<String, StratumJob>) -> Option<String> {
    jobs.keys()
        .min_by(|left, right| {
            stratum_job_id_sort_key(left)
                .cmp(&stratum_job_id_sort_key(right))
                .then_with(|| left.cmp(right))
        })
        .cloned()
}

fn stratum_job_id_sort_key(job_id: &str) -> u64 {
    job_id.parse().unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use crate::{app::ExternalMineJob, domain::StratumMineTemplate};

    use super::{
        STRATUM_MAX_JOBS_PER_SESSION, STRATUM_MAX_LINE_BYTES, STRATUM_MAX_SESSIONS, StratumJob,
        StratumLineReader, StratumSessionLimiter, insert_bounded_job,
    };

    fn dummy_job() -> StratumJob {
        StratumJob {
            mine: ExternalMineJob {
                template: StratumMineTemplate {
                    recipient: "0".repeat(64),
                    anchor: "0".repeat(64),
                    salt: 0,
                    difficulty_bits: 0,
                    coinbase_prefix: Vec::new(),
                    version_hex: "00000000".to_string(),
                    prev_hash_hex: "0".repeat(64),
                    nbits_hex: "00000000".to_string(),
                    ntime_hex: "00000000".to_string(),
                },
            },
        }
    }

    #[tokio::test]
    async fn stratum_line_reader_accepts_max_sized_line() {
        let (mut client, server) = tokio::io::duplex(STRATUM_MAX_LINE_BYTES + 1);
        let mut reader = StratumLineReader::new(server);
        let line = vec![b'a'; STRATUM_MAX_LINE_BYTES];
        client.write_all(&line).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        let read = reader.read_line().await.unwrap().unwrap();

        assert_eq!(read.len(), STRATUM_MAX_LINE_BYTES);
    }

    #[tokio::test]
    async fn stratum_line_reader_rejects_oversized_line() {
        let (mut client, server) = tokio::io::duplex(STRATUM_MAX_LINE_BYTES + 2);
        let mut reader = StratumLineReader::new(server);
        let line = vec![b'a'; STRATUM_MAX_LINE_BYTES + 1];
        client.write_all(&line).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        let error = reader.read_line().await.unwrap_err();

        assert!(error.to_string().contains("Stratum request exceeds"));
    }

    #[test]
    fn stratum_job_cache_prunes_oldest_jobs() {
        let mut jobs = std::collections::BTreeMap::new();
        for id in 1..=STRATUM_MAX_JOBS_PER_SESSION + 2 {
            insert_bounded_job(&mut jobs, id.to_string(), dummy_job());
        }

        assert_eq!(jobs.len(), STRATUM_MAX_JOBS_PER_SESSION);
        assert!(!jobs.contains_key("1"));
        assert!(!jobs.contains_key("2"));
        assert!(jobs.contains_key("3"));
    }

    #[test]
    fn stratum_session_limiter_enforces_global_cap() {
        let limiter = StratumSessionLimiter::new(STRATUM_MAX_SESSIONS);
        let permits = (0..STRATUM_MAX_SESSIONS)
            .map(|_| limiter.try_acquire().expect("permit should be available"))
            .collect::<Vec<_>>();

        assert!(limiter.try_acquire().is_none());
        drop(permits);
        assert!(limiter.try_acquire().is_some());
    }
}
