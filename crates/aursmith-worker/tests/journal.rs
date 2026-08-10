use aursmith_domain::{AttemptRef, WorkerRole};
use aursmith_protocol::{JobSpec, ResourceLimits, SignedEnvelope};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration as StdDuration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
    time::sleep,
};
use uuid::Uuid;

struct RunningWorker {
    child: Child,
    directory: TempDir,
    socket: std::path::PathBuf,
    key: SigningKey,
}

impl RunningWorker {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("worker.sock");
        let database = directory.path().join("worker.db");
        let key = SigningKey::from_bytes(&[11_u8; 32]);
        let child = Command::new(env!("CARGO_BIN_EXE_aursmith-worker"))
            .arg("--name")
            .arg("test-builder")
            .arg("--role")
            .arg("builder")
            .arg("--socket")
            .arg(&socket)
            .arg("--database")
            .arg(format!("sqlite://{}", database.display()))
            .arg("--controller-verifying-key-hex")
            .arg(hex::encode(key.verifying_key().to_bytes()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if socket.exists() {
                return Self {
                    child,
                    directory,
                    socket,
                    key,
                };
            }
            sleep(StdDuration::from_millis(20)).await;
        }
        panic!("Worker 没有创建 Unix Socket");
    }

    async fn send(&self, command: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).await.unwrap();
        let mut bytes = serde_json::to_vec(&command).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).await.unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn envelope(&self, job_id: Uuid, generation: u32) -> SignedEnvelope {
        let now = Utc::now();
        let spec = JobSpec {
            job_id,
            attempt: AttemptRef {
                job_id,
                attempt_id: Uuid::new_v4(),
                generation,
            },
            required_role: WorkerRole::Builder,
            revision_sha256: "a".repeat(64),
            source_manifest_sha256: None,
            dependency_snapshot_sha256: None,
            profile_sha256: None,
            inputs: Vec::new(),
            limits: ResourceLimits {
                cpu_count: 1,
                memory_mib: 1024,
                disk_mib: 4096,
                timeout_seconds: 600,
            },
            issued_at: now,
            expires_at: now + Duration::minutes(5),
        };
        SignedEnvelope::sign("aursmith.job_spec", &spec, &self.key).unwrap()
    }
}

impl Drop for RunningWorker {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        assert!(self.directory.path().exists());
    }
}

#[tokio::test]
async fn journal_accepts_idempotent_replay_and_rejects_stale_attempt() {
    let worker = RunningWorker::start().await;
    let job_id = Uuid::new_v4();
    let first = worker.envelope(job_id, 0);
    let accepted = worker
        .send(json!({"command": "submit", "envelope": first}))
        .await;
    assert_eq!(accepted["code"], "ACCEPTED");

    let replay = worker
        .send(json!({"command": "submit", "envelope": first}))
        .await;
    assert_eq!(replay["code"], "IDEMPOTENT_REPLAY");

    let newer = worker.envelope(job_id, 1);
    let accepted = worker
        .send(json!({"command": "submit", "envelope": newer}))
        .await;
    assert_eq!(accepted["code"], "ACCEPTED");

    let stale = worker
        .send(json!({"command": "submit", "envelope": first}))
        .await;
    assert_eq!(stale["code"], "STALE_ATTEMPT");
}

#[tokio::test]
async fn worker_rejects_self_signed_untrusted_controller() {
    let worker = RunningWorker::start().await;
    let job_id = Uuid::new_v4();
    let now = Utc::now();
    let untrusted = SigningKey::from_bytes(&[12_u8; 32]);
    let spec = JobSpec {
        job_id,
        attempt: AttemptRef {
            job_id,
            attempt_id: Uuid::new_v4(),
            generation: 0,
        },
        required_role: WorkerRole::Builder,
        revision_sha256: "b".repeat(64),
        source_manifest_sha256: None,
        dependency_snapshot_sha256: None,
        profile_sha256: None,
        inputs: Vec::new(),
        limits: ResourceLimits {
            cpu_count: 1,
            memory_mib: 1024,
            disk_mib: 4096,
            timeout_seconds: 60,
        },
        issued_at: now,
        expires_at: now + Duration::minutes(1),
    };
    let envelope = SignedEnvelope::sign("aursmith.job_spec", &spec, &untrusted).unwrap();
    let response = worker
        .send(json!({"command": "submit", "envelope": envelope}))
        .await;
    assert_eq!(response["code"], "UNTRUSTED_CONTROLLER");
}
