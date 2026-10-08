//! Builder 端到端测试：内存版 aursmithd Builder API + 假 docker 脚本。

use super::*;
use crate::config::test_config;
use aursmith_core::protocol::{DependencyArtifact, InputFile, LeaseResponse, UploadStatus};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

const TOKEN: &str = "builder-token-builder-token-builder-token";

#[derive(Default)]
struct Mock {
    queue: Vec<BuildSpec>,
    reports: Vec<(String, BuildReport)>,
    uploads: HashMap<String, Vec<u8>>,
    completed: Vec<String>,
    dependency_bytes: Vec<u8>,
    reject_report: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn error(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(serde_json::json!({"code": code, "message": code})),
    )
        .into_response()
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {TOKEN}"))
}

async fn lease(State(mock): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return error(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    }
    let build = mock.lock().unwrap().queue.pop();
    Json(LeaseResponse { build }).into_response()
}

async fn report(
    State(mock): State<Shared>,
    AxumPath(id): AxumPath<String>,
    Json(report): Json<BuildReport>,
) -> Response {
    let mut mock = mock.lock().unwrap();
    if mock.reject_report {
        return error(StatusCode::CONFLICT, "LEASE_NOT_ACTIVE");
    }
    let state = if report.outcome == BuildOutcome::Succeeded {
        "uploading"
    } else {
        "failed"
    };
    mock.reports.push((id.clone(), report));
    Json(serde_json::json!({"build_id": id, "state": state})).into_response()
}

fn artifact_of(mock: &Mock, id: &str, file: &str) -> Option<ArtifactRecord> {
    mock.reports
        .iter()
        .find(|(build, _)| build == id)
        .and_then(|(_, report)| {
            report
                .artifacts
                .iter()
                .find(|artifact| artifact.file == file)
                .cloned()
        })
}

fn status_of(mock: &Mock, artifact: &ArtifactRecord) -> UploadStatus {
    let received = mock
        .uploads
        .get(&artifact.file)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0);
    UploadStatus {
        file: artifact.file.clone(),
        size: artifact.size,
        received,
        complete: received == artifact.size,
    }
}

async fn upload_get(
    State(mock): State<Shared>,
    AxumPath((id, file)): AxumPath<(String, String)>,
) -> Response {
    let mock = mock.lock().unwrap();
    match artifact_of(&mock, &id, &file) {
        Some(artifact) => Json(status_of(&mock, &artifact)).into_response(),
        None => error(StatusCode::NOT_FOUND, "NOT_FOUND"),
    }
}

#[derive(serde::Deserialize)]
struct Offset {
    offset: u64,
}

async fn upload_put(
    State(mock): State<Shared>,
    AxumPath((id, file)): AxumPath<(String, String)>,
    Query(query): Query<Offset>,
    body: Bytes,
) -> Response {
    let mut mock = mock.lock().unwrap();
    let Some(artifact) = artifact_of(&mock, &id, &file) else {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND");
    };
    let entry = mock.uploads.entry(file.clone()).or_default();
    if query.offset != entry.len() as u64 {
        return error(StatusCode::CONFLICT, "OFFSET_MISMATCH");
    }
    entry.extend_from_slice(&body);
    if entry.len() as u64 == artifact.size && aursmith_core::sha256_hex(entry) != artifact.sha256 {
        entry.clear();
        return error(StatusCode::BAD_REQUEST, "DIGEST_MISMATCH");
    }
    Json(status_of(&mock, &artifact)).into_response()
}

async fn complete(State(mock): State<Shared>, AxumPath(id): AxumPath<String>) -> Response {
    mock.lock().unwrap().completed.push(id.clone());
    Json(serde_json::json!({"build_id": id, "state": "succeeded"})).into_response()
}

async fn artifact(
    State(mock): State<Shared>,
    AxumPath((_id, _file)): AxumPath<(String, String)>,
) -> Response {
    mock.lock()
        .unwrap()
        .dependency_bytes
        .clone()
        .into_response()
}

async fn serve(mock: Shared) -> String {
    let app = Router::new()
        .route("/api/v1/builder/lease", post(lease))
        .route("/api/v1/builder/builds/{id}/report", post(report))
        .route(
            "/api/v1/builder/builds/{id}/files/{file}",
            get(upload_get).put(upload_put),
        )
        .route("/api/v1/builder/builds/{id}/complete", post(complete))
        .route("/api/v1/builder/artifacts/{id}/{file}", get(artifact))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

/// 假 docker：解析挂载参数，检查依赖文件，按 MODE 写出成功产物或 guest-error.json。
fn fake_docker(root: &Path, mode: &str) -> PathBuf {
    let script = root.join("docker");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
[ "$1" = "rm" ] && exit 0
for arg in "$@"; do
  case "$arg" in
    *:/mnt/aursmith-input:ro) input="${{arg%%:/mnt/aursmith-input:ro}}" ;;
    *:/mnt/aursmith-output:rw) output="${{arg%%:/mnt/aursmith-output:rw}}" ;;
    aursmith-build-*) id="${{arg#aursmith-build-}}" ;;
  esac
done
echo "fake docker for $id"
test -f "$input/PKGBUILD" || exit 9
for dep in "$input"/.aursmith/deps/*; do [ -e "$dep" ] && echo "dep $(basename "$dep")"; done
if [ "{mode}" = "fail" ]; then
  echo '==> ERROR: One or more files did not pass the validity check!' > "$output/build.log"
  printf '{{"code":"GUEST_CHECKSUM_FAILED","error":"checksum"}}' > "$output/guest-error.json"
  exit 1
fi
head -c 100 /dev/zero | tr '\0' 'x' > "$output/demo-1-1-any.pkg.tar.zst"
sha=$(sha256sum "$output/demo-1-1-any.pkg.tar.zst" | cut -d' ' -f1)
echo 'build ok' > "$output/build.log"
printf '{{"build_id":"%s","artifacts":[{{"file":"demo-1-1-any.pkg.tar.zst","sha256":"%s","size":100,"package_name":"demo","package_version":"1-1","architecture":"any"}}]}}' "$id" "$sha" > "$output/guest-result.json"
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn input(path: &str, content: &[u8]) -> InputFile {
    InputFile {
        path: path.into(),
        sha256: aursmith_core::sha256_hex(content),
        size: content.len() as u64,
        content_base64: STANDARD.encode(content),
    }
}

fn spec(build_id: &str, dependencies: Vec<DependencyArtifact>) -> BuildSpec {
    BuildSpec {
        build_id: build_id.into(),
        attempt: 1,
        package_base: "demo".into(),
        revision_id: "rev".into(),
        aur_commit: "a".repeat(40),
        vcs_commit: None,
        tree_sha256: "b".repeat(64),
        expected_outputs: vec!["demo".into()],
        allow_check: false,
        files: vec![
            input("PKGBUILD", b"pkgname=demo\npkgver=1\npkgrel=1\n"),
            input(
                ".SRCINFO",
                b"pkgbase = demo\n\tpkgver = 1\n\tpkgrel = 1\npkgname = demo\n",
            ),
            input("patches/fix.patch", b"--- a\n+++ b\n"),
        ],
        dependencies,
        lease_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
    }
}

struct Harness {
    _root: tempfile::TempDir,
    config: BuilderConfig,
    client: Client,
    mock: Shared,
}

async fn harness(mode: &str) -> Harness {
    let root = tempfile::tempdir().unwrap();
    let mock: Shared = Arc::default();
    let url = serve(mock.clone()).await;
    let docker = fake_docker(root.path(), mode);
    let config = test_config(&url, root.path().join("jobs"), docker);
    let client = Client::new(config.server_url.clone(), TOKEN.into()).unwrap();
    Harness {
        _root: root,
        config,
        client,
        mock,
    }
}

#[tokio::test]
async fn successful_build_is_reported_uploaded_and_completed() {
    let harness = harness("ok").await;
    let dependency = b"dependency package".to_vec();
    harness.mock.lock().unwrap().dependency_bytes = dependency.clone();
    harness.mock.lock().unwrap().queue.push(spec(
        "11111111-aaaa",
        vec![DependencyArtifact {
            build_id: "22222222-bbbb".into(),
            file: "dep-1-1-any.pkg.tar.zst".into(),
            sha256: aursmith_core::sha256_hex(&dependency),
            size: dependency.len() as u64,
        }],
    ));
    let leased = harness.client.lease("test", 3600).await.unwrap().unwrap();
    execute(&harness.config, &harness.client, leased)
        .await
        .unwrap();
    let mock = harness.mock.lock().unwrap();
    let (id, report) = &mock.reports[0];
    assert_eq!(id, "11111111-aaaa");
    assert_eq!(report.outcome, BuildOutcome::Succeeded);
    assert_eq!(report.artifacts.len(), 1);
    assert_eq!(report.artifacts[0].package_name, "demo");
    assert!(report.log.contains("build ok"));
    assert!(report.log.contains("dep dep-1-1-any.pkg.tar.zst"));
    assert_eq!(mock.uploads["demo-1-1-any.pkg.tar.zst"], vec![b'x'; 100]);
    assert_eq!(mock.completed, ["11111111-aaaa"]);
    assert!(!harness.config.jobs_dir.join("11111111-aaaa").exists());
}

#[tokio::test]
async fn guest_failure_is_reported_with_its_code() {
    let harness = harness("fail").await;
    execute(&harness.config, &harness.client, spec("33333333", vec![]))
        .await
        .unwrap();
    let mock = harness.mock.lock().unwrap();
    let (_, report) = &mock.reports[0];
    assert_eq!(report.outcome, BuildOutcome::Failed);
    assert_eq!(report.error_code.as_deref(), Some("GUEST_CHECKSUM_FAILED"));
    assert!(report.log.contains("validity check"));
    assert!(mock.completed.is_empty());
}

#[tokio::test]
async fn tampered_inputs_and_dependencies_fail_before_docker() {
    let harness = harness("ok").await;
    let mut bad = spec("44444444", vec![]);
    bad.files[0].sha256 = "0".repeat(64);
    execute(&harness.config, &harness.client, bad)
        .await
        .unwrap();
    let mut reserved = spec("55555555", vec![]);
    reserved.files.push(input(".aursmith/guest.json", b"{}"));
    execute(&harness.config, &harness.client, reserved)
        .await
        .unwrap();
    harness.mock.lock().unwrap().dependency_bytes = b"evil".to_vec();
    let tampered = spec(
        "66666666",
        vec![DependencyArtifact {
            build_id: "77777777".into(),
            file: "dep-1-1-any.pkg.tar.zst".into(),
            sha256: aursmith_core::sha256_hex(b"good"),
            size: 4,
        }],
    );
    execute(&harness.config, &harness.client, tampered)
        .await
        .unwrap();
    let mock = harness.mock.lock().unwrap();
    let codes: Vec<_> = mock
        .reports
        .iter()
        .map(|(_, report)| report.error_code.clone().unwrap())
        .collect();
    assert_eq!(
        codes,
        [
            "INPUT_INVALID",
            "INPUT_INVALID",
            "DEPENDENCY_DOWNLOAD_FAILED"
        ]
    );
    assert!(
        mock.reports
            .iter()
            .all(|(_, report)| !report.log.contains("fake docker"))
    );
}

#[tokio::test]
async fn uploads_resume_from_the_server_offset_in_chunks() {
    let harness = harness("ok").await;
    let root = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..100_u8).collect();
    let path = root.path().join("demo-1-1-any.pkg.tar.zst");
    fs::write(&path, &content).unwrap();
    let artifact = ArtifactRecord {
        file: "demo-1-1-any.pkg.tar.zst".into(),
        sha256: aursmith_core::sha256_hex(&content),
        size: 100,
        package_name: "demo".into(),
        package_version: "1-1".into(),
        architecture: "any".into(),
    };
    {
        let mut mock = harness.mock.lock().unwrap();
        mock.reports.push((
            "88888888".into(),
            BuildReport {
                attempt: 1,
                outcome: BuildOutcome::Succeeded,
                artifacts: vec![artifact.clone()],
                error_code: None,
                log: String::new(),
                log_truncated: false,
            },
        ));
        // 模拟上次中断前已上传 10 字节。
        mock.uploads
            .insert(artifact.file.clone(), content[..10].to_vec());
    }
    upload_file(&harness.client, "88888888", &path, &artifact, 7)
        .await
        .unwrap();
    assert_eq!(
        harness.mock.lock().unwrap().uploads[&artifact.file],
        content
    );
}

#[tokio::test]
async fn rejected_report_discards_the_job_directory() {
    let harness = harness("ok").await;
    harness.mock.lock().unwrap().reject_report = true;
    execute(&harness.config, &harness.client, spec("99999999", vec![]))
        .await
        .unwrap();
    assert!(!harness.config.jobs_dir.join("99999999").exists());
    assert!(harness.mock.lock().unwrap().completed.is_empty());
}

#[tokio::test]
async fn interrupted_builds_are_reported_as_builder_restarted() {
    let harness = harness("ok").await;
    let dir = harness.config.jobs_dir.join("aaaaaaaa");
    fs::create_dir_all(dir.join("input")).unwrap();
    write_json(
        &dir.join("state.json"),
        &JobState {
            build_id: "aaaaaaaa".into(),
            attempt: 2,
        },
    )
    .unwrap();
    recover(&harness.config, &harness.client).await;
    let mock = harness.mock.lock().unwrap();
    let (id, report) = &mock.reports[0];
    assert_eq!(id, "aaaaaaaa");
    assert_eq!(report.attempt, 2);
    assert_eq!(report.error_code.as_deref(), Some("BUILDER_RESTARTED"));
    assert!(!dir.exists());
}

#[test]
fn build_container_gets_read_only_inputs_and_no_credentials() {
    let config = test_config(
        "https://aursmith.example",
        "/var/lib/aursmith-builder/jobs".into(),
        "/usr/bin/docker".into(),
    );
    let arguments = docker_arguments(
        &config,
        "abc",
        Path::new("/var/lib/aursmith-builder/jobs/abc"),
    );
    let joined = arguments.join(" ");
    assert!(joined.contains("/var/lib/aursmith-builder/jobs/abc/input:/mnt/aursmith-input:ro"));
    assert!(joined.contains("--memory 512m"));
    assert!(!joined.contains(&config.token));
    assert!(!joined.contains("docker.sock"));
    assert!(!joined.contains("--privileged"));
    assert_eq!(arguments.last().unwrap(), "aursmith-build:latest");
}

#[test]
fn docker_diagnostics_are_classified() {
    assert_eq!(
        classify_docker_diagnostic("Cannot connect to the Docker daemon"),
        "DOCKER_DAEMON_UNAVAILABLE"
    );
    assert_eq!(
        classify_docker_diagnostic("Unable to find image 'aursmith-build:latest' locally"),
        "BUILD_IMAGE_MISSING"
    );
    assert_eq!(
        classify_docker_diagnostic("no space left on device"),
        "DOCKER_DISK_FULL"
    );
    assert_eq!(truncate_tail("αβγ".into(), 3), ("γ".to_owned(), true));
}
