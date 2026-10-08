//! 订阅、AUR 同步、revision 生成与依赖闭包。
//!
//! 订阅表只保存直接订阅；依赖闭包从每个 pkgbase 最新已批准（或尚在审查中的）revision
//! 的依赖解析结果实时计算，不再维护引用计数。

use crate::{
    app::AppState,
    aur::{AurPackage, Snapshot},
    error::ApiError,
};
use aursmith_core::{
    graph::DependencyGraph,
    names::validate_package_name,
    scan::{FindingSeverity, ScanFile, scan_aur_wrapper},
    srcinfo::{SrcInfo, git_vcs_source},
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

/// 闭包中最多允许的 pkgbase 数量，防止依赖爆炸。
const MAXIMUM_CLOSURE: usize = 256;
/// 每个 revision 最多解析的依赖名数量。
const MAXIMUM_DEPENDENCIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDependency {
    pub name: String,
    pub kind: String,
    /// `official`、`aur`、`needs_selection` 或 `unknown`。
    pub state: String,
    pub target: Option<String>,
    pub candidates: Vec<String>,
}

// ------------------------------------------------------------------ 闭包

#[derive(Debug, Clone, Default)]
pub struct Closure {
    pub direct: BTreeSet<String>,
    pub members: BTreeSet<String>,
    /// pkgbase -> 依赖的 AUR pkgbase（已应用 provider 选择）。
    pub graph: DependencyGraph,
    /// pkgbase -> 未解决的 provider 选择（依赖名）。
    pub unresolved: BTreeMap<String, Vec<String>>,
    /// pkgbase -> 依赖它的 pkgbase。
    pub required_by: BTreeMap<String, BTreeSet<String>>,
}

/// 依赖解析使用的 revision：最新已批准的；没有则用最新仍在审查中的（让依赖可以并行同步与审查）。
pub async fn dependency_revision(
    db: &SqlitePool,
    package_base: &str,
) -> sqlx::Result<Option<(String, Vec<ResolvedDependency>)>> {
    let row = sqlx::query(
        "SELECT id, dependencies_json FROM revisions WHERE package_base = ? AND state IN ('approved', 'pending_review', 'manual_review') \
         ORDER BY CASE state WHEN 'approved' THEN 0 ELSE 1 END, created_at DESC LIMIT 1",
    )
    .bind(package_base)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| {
        (
            row.get("id"),
            serde_json::from_str(row.get("dependencies_json")).unwrap_or_default(),
        )
    }))
}

pub async fn compute_closure(db: &SqlitePool) -> sqlx::Result<Closure> {
    let direct: BTreeSet<String> = sqlx::query_scalar("SELECT package_base FROM subscriptions")
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    let choices: BTreeMap<(String, String), String> =
        sqlx::query("SELECT package_base, dependency, provider_base FROM provider_choices")
            .fetch_all(db)
            .await?
            .into_iter()
            .map(|row| {
                (
                    (row.get("package_base"), row.get("dependency")),
                    row.get("provider_base"),
                )
            })
            .collect();
    let mut closure = Closure {
        direct: direct.clone(),
        ..Closure::default()
    };
    let mut queue: VecDeque<String> = direct.into_iter().collect();
    while let Some(base) = queue.pop_front() {
        if !closure.members.insert(base.clone()) {
            continue;
        }
        closure.graph.add_package(base.clone());
        if closure.members.len() > MAXIMUM_CLOSURE {
            tracing::warn!("依赖闭包超过 {MAXIMUM_CLOSURE} 个 pkgbase，截断");
            break;
        }
        let Some((_, dependencies)) = dependency_revision(db, &base).await? else {
            continue;
        };
        for dependency in dependencies {
            let target = match dependency.state.as_str() {
                "aur" => dependency.target.clone(),
                "needs_selection" => {
                    let chosen = choices
                        .get(&(base.clone(), dependency.name.clone()))
                        .cloned();
                    if chosen.is_none() {
                        closure
                            .unresolved
                            .entry(base.clone())
                            .or_default()
                            .push(dependency.name.clone());
                    }
                    chosen
                }
                _ => None,
            };
            if let Some(target) = target.filter(|target| target != &base) {
                closure.graph.add_dependency(base.clone(), target.clone());
                closure
                    .required_by
                    .entry(target.clone())
                    .or_default()
                    .insert(base.clone());
                if !closure.members.contains(&target) {
                    queue.push_back(target);
                }
            }
        }
    }
    Ok(closure)
}

// ------------------------------------------------------------------ revision 生成

fn scan_files(snapshot: &Snapshot) -> Vec<ScanFile> {
    snapshot
        .files
        .iter()
        .map(|file| ScanFile {
            path: file.path.clone(),
            declared_sha256: file.sha256.clone(),
            binary: file.binary,
            bytes: file.bytes().unwrap_or_default(),
        })
        .collect()
}

/// 把一个新快照落库为 revision。已存在相同 (pkgbase, aur_commit, vcs_commit) 时返回 None。
///
/// - 确定性扫描出现 Block → `rejected`；
/// - 与某个已批准 revision 的文件树逐字节相同（例如只有 VCS 上游前进）→ 复用审查，直接 `approved`；
/// - 否则 `pending_review`，交给 2+1 审查循环。
pub async fn insert_revision(
    db: &SqlitePool,
    snapshot: &Snapshot,
    dependencies: &[ResolvedDependency],
) -> anyhow::Result<Option<(String, String)>> {
    let mut transaction = db.begin().await?;
    let exists: Option<String> = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND aur_commit = ? AND COALESCE(vcs_commit, '') = COALESCE(?, '')",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .fetch_optional(&mut *transaction)
    .await?;
    if exists.is_some() {
        return Ok(None);
    }
    let now = Utc::now();
    ensure_package(&mut transaction, &snapshot.package_base, now).await?;
    let tree = snapshot.tree_sha256();
    let baseline: Option<String> = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND state = 'approved' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&snapshot.package_base)
    .fetch_optional(&mut *transaction)
    .await?;
    let reusable: Option<String> = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND state = 'approved' AND tree_sha256 = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&snapshot.package_base)
    .bind(&tree)
    .fetch_optional(&mut *transaction)
    .await?;
    let findings = scan_aur_wrapper(&scan_files(snapshot));
    let blocked = findings
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Block);
    let state = if blocked {
        "rejected"
    } else if reusable.is_some() {
        "approved"
    } else {
        "pending_review"
    };
    let id = Uuid::new_v4().to_string();
    sqlx::query("UPDATE revisions SET state = 'superseded' WHERE package_base = ? AND state IN ('pending_review', 'manual_review')")
        .bind(&snapshot.package_base)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO revisions(id, package_base, aur_commit, vcs_commit, version, tree_sha256, snapshot_json, dependencies_json, baseline_revision_id, state, created_at, decided_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .bind(&snapshot.info.version)
    .bind(&tree)
    .bind(serde_json::to_string(snapshot)?)
    .bind(serde_json::to_string(dependencies)?)
    .bind(&baseline)
    .bind(state)
    .bind(now)
    .bind((state != "pending_review").then_some(now))
    .execute(&mut *transaction)
    .await?;
    sqlx::query("INSERT INTO reviews(id, revision_id, kind, verdict, summary, findings_json, created_at) VALUES (?, ?, 'scan', ?, ?, ?, ?)")
        .bind(Uuid::new_v4().to_string())
        .bind(&id)
        .bind(if blocked { "reject" } else { "approve" })
        .bind(if blocked { "确定性扫描发现 Block 级问题，直接拒绝" } else { "确定性扫描未发现 Block 级问题" })
        .bind(serde_json::to_string(&findings)?)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    if let (false, Some(source)) = (blocked, &reusable) {
        sqlx::query("INSERT INTO reviews(id, revision_id, kind, verdict, summary, created_at) VALUES (?, ?, 'reuse', 'approve', ?, ?)")
            .bind(Uuid::new_v4().to_string())
            .bind(&id)
            .bind(format!("AUR 包装层与已批准 revision {source} 逐字节相同，复用其审查结论"))
            .bind(now)
            .execute(&mut *transaction)
            .await?;
    }
    for target in dependencies
        .iter()
        .filter_map(|dependency| dependency.target.as_ref())
    {
        ensure_package(&mut transaction, target, now).await?;
    }
    sqlx::query("UPDATE packages SET version = COALESCE(version, ?), outputs_json = ? WHERE package_base = ?")
        .bind(&snapshot.info.version)
        .bind(serde_json::to_string(&snapshot.info.outputs)?)
        .bind(&snapshot.package_base)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(Some((id, state.to_owned())))
}

async fn ensure_package(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    package_base: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO packages(package_base, next_check_at) VALUES (?, ?)")
        .bind(package_base)
        .bind(now)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// 解析依赖：AUR 包名 → pkgbase；否则查官方仓库（含 provides）；否则查 AUR provides。
pub async fn resolve_dependencies(
    state: &AppState,
    info: &SrcInfo,
) -> anyhow::Result<Vec<ResolvedDependency>> {
    let own: BTreeSet<&String> = info.outputs.iter().chain(&info.provides).collect();
    let dependencies: Vec<_> = info
        .dependencies
        .iter()
        .filter(|dependency| !own.contains(&dependency.name))
        .take(MAXIMUM_DEPENDENCIES)
        .collect();
    let names: Vec<String> = dependencies
        .iter()
        .map(|dependency| dependency.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| validate_package_name(name).is_ok())
        .collect();
    let mut aur_bases = BTreeMap::new();
    for chunk in names.chunks(100) {
        for package in state.aur.info(chunk).await? {
            aur_bases.insert(package.name.clone(), package.package_base.clone());
        }
    }
    let mut resolved_names: BTreeMap<String, (String, Option<String>, Vec<String>)> =
        BTreeMap::new();
    for name in &names {
        let resolution = if let Some(base) = aur_bases.get(name) {
            ("aur".to_owned(), Some(base.clone()), Vec::new())
        } else if !state.aur.official(name).await?.is_empty() {
            ("official".to_owned(), None, Vec::new())
        } else {
            let candidates: Vec<String> = state
                .aur
                .providers(name)
                .await?
                .into_iter()
                .map(|package| package.package_base)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            match candidates.len() {
                0 => ("unknown".to_owned(), None, Vec::new()),
                1 => ("aur".to_owned(), candidates.first().cloned(), candidates),
                _ => ("needs_selection".to_owned(), None, candidates),
            }
        };
        resolved_names.insert(name.clone(), resolution);
    }
    Ok(dependencies
        .into_iter()
        .map(|dependency| {
            let (state, target, candidates) = resolved_names
                .get(&dependency.name)
                .cloned()
                .unwrap_or_else(|| ("unknown".into(), None, Vec::new()));
            ResolvedDependency {
                name: dependency.name.clone(),
                kind: dependency.kind.clone(),
                state,
                target,
                candidates,
            }
        })
        .collect())
}

/// 抓取快照、解析依赖并落库。
pub async fn ingest(
    state: &AppState,
    package_base: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let snapshot = state.aur.snapshot(package_base).await?;
    let exists: Option<String> = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND aur_commit = ? AND COALESCE(vcs_commit, '') = COALESCE(?, '')",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_some() {
        return Ok(None);
    }
    let dependencies = resolve_dependencies(state, &snapshot.info).await?;
    let created = insert_revision(&state.db, &snapshot, &dependencies).await?;
    if created.is_some() {
        state.wake();
    }
    Ok(created)
}

// ------------------------------------------------------------------ 周期同步

pub fn next_scheduled_check(
    package_base: &str,
    after: DateTime<Utc>,
    interval_minutes: i64,
) -> DateTime<Utc> {
    let period = interval_minutes.max(1) * 60;
    let digest = Sha256::digest(package_base.as_bytes());
    let prefix = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 前缀长度固定"));
    let offset = i64::try_from(prefix % u64::try_from(period).unwrap_or(1)).unwrap_or(0);
    let cycle = (after.timestamp() - offset).div_euclid(period) + 1;
    DateTime::from_timestamp(cycle * period + offset, 0)
        .unwrap_or(after + Duration::seconds(period))
}

async fn record_sync(
    state: &AppState,
    package_base: &str,
    result: Result<(), String>,
) -> sqlx::Result<()> {
    let now = Utc::now();
    match result {
        Ok(()) => {
            sqlx::query("UPDATE packages SET last_checked_at = ?, next_check_at = ?, sync_failures = 0, sync_error = NULL WHERE package_base = ?")
                .bind(now)
                .bind(next_scheduled_check(package_base, now, state.config.update_interval_minutes))
                .bind(package_base)
                .execute(&state.db)
                .await?;
        }
        Err(error) => {
            let failures: i64 =
                sqlx::query_scalar("SELECT sync_failures FROM packages WHERE package_base = ?")
                    .bind(package_base)
                    .fetch_optional(&state.db)
                    .await?
                    .unwrap_or(0);
            let delay = (30 * (failures + 1)).min(state.config.update_interval_minutes.max(30));
            sqlx::query("UPDATE packages SET last_checked_at = ?, next_check_at = ?, sync_failures = sync_failures + 1, sync_error = ? WHERE package_base = ?")
                .bind(now)
                .bind(now + Duration::minutes(delay))
                .bind(error.chars().take(2000).collect::<String>())
                .bind(package_base)
                .execute(&state.db)
                .await?;
        }
    }
    Ok(())
}

async fn update_cache(db: &SqlitePool, package: &AurPackage) -> sqlx::Result<()> {
    sqlx::query("UPDATE packages SET version = ?, description = ?, maintainer = ?, out_of_date = ?, last_modified = ? WHERE package_base = ?")
        .bind(&package.version)
        .bind(&package.description)
        .bind(&package.maintainer)
        .bind(package.out_of_date)
        .bind(package.last_modified)
        .bind(&package.package_base)
        .execute(db)
        .await?;
    Ok(())
}

/// 检查一个 pkgbase：AUR 元数据变化或 VCS 上游前进时抓取新快照。
pub async fn sync_package(
    state: &AppState,
    package_base: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let row =
        sqlx::query("SELECT outputs_json, last_modified FROM packages WHERE package_base = ?")
            .bind(package_base)
            .fetch_one(&state.db)
            .await?;
    let outputs: Vec<String> = serde_json::from_str(row.get("outputs_json")).unwrap_or_default();
    let previous_modified: Option<i64> = row.get("last_modified");
    let lookup = outputs
        .first()
        .cloned()
        .unwrap_or_else(|| package_base.to_owned());
    let package = state
        .aur
        .info(&[lookup])
        .await?
        .into_iter()
        .find(|package| package.package_base == package_base)
        .ok_or_else(|| anyhow::anyhow!("AUR 中已不存在 pkgbase {package_base}"))?;
    update_cache(&state.db, &package).await?;
    let latest = sqlx::query("SELECT vcs_commit, snapshot_json FROM revisions WHERE package_base = ? ORDER BY created_at DESC LIMIT 1")
        .bind(package_base)
        .fetch_optional(&state.db)
        .await?;
    let changed = match &latest {
        None => true,
        Some(_) if previous_modified != Some(package.last_modified) => true,
        Some(latest) => {
            let snapshot: Snapshot = serde_json::from_str(latest.get("snapshot_json"))?;
            match git_vcs_source(&snapshot.info.sources) {
                Some(_) => {
                    state.aur.vcs_commit(&snapshot.info.sources).await?
                        != latest.get::<Option<String>, _>("vcs_commit")
                }
                None => false,
            }
        }
    };
    if changed {
        ingest(state, package_base).await
    } else {
        Ok(None)
    }
}

/// 后台同步循环：只检查当前闭包内到期的 pkgbase。
pub async fn run_sync_loop(state: AppState) {
    loop {
        if let Err(error) = sync_due(&state).await {
            tracing::error!(error = %error, "AUR 同步循环失败");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

async fn sync_due(state: &AppState) -> anyhow::Result<()> {
    let closure = compute_closure(&state.db).await?;
    let due: Vec<String> = sqlx::query_scalar(
        "SELECT package_base FROM packages WHERE next_check_at <= ? ORDER BY next_check_at",
    )
    .bind(Utc::now())
    .fetch_all(&state.db)
    .await?;
    for package_base in due
        .into_iter()
        .filter(|base| closure.members.contains(base))
    {
        let result = sync_package(state, &package_base).await;
        match &result {
            Ok(Some((revision, revision_state))) => {
                tracing::info!(%package_base, %revision, %revision_state, "发现新 revision")
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%package_base, error = %error, "同步失败"),
        }
        record_sync(
            state,
            &package_base,
            result.map(|_| ()).map_err(|error| format!("{error:#}")),
        )
        .await?;
    }
    Ok(())
}

// ------------------------------------------------------------------ HTTP API

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

fn package_json(package: &AurPackage) -> Value {
    json!({
        "name": package.name,
        "package_base": package.package_base,
        "version": package.version,
        "description": package.description,
        "maintainer": package.maintainer,
        "out_of_date": package.out_of_date,
        "last_modified": package.last_modified,
        "depends": package.depends,
        "make_depends": package.make_depends,
        "check_depends": package.check_depends,
        "opt_depends": package.opt_depends,
        "provides": package.provides,
    })
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let results = state
        .aur
        .search(&query.q)
        .await
        .map_err(|error| ApiError::bad_request("AUR_SEARCH_FAILED", format!("{error:#}")))?;
    Ok(Json(
        json!({"items": results.iter().map(package_json).collect::<Vec<_>>()}),
    ))
}

pub async fn list_subscriptions(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let closure = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let mut items = Vec::new();
    for base in &closure.members {
        let package = sqlx::query("SELECT version, description, maintainer, out_of_date, outputs_json, sync_error, last_checked_at FROM packages WHERE package_base = ?")
            .bind(base)
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::internal)?;
        let revision = sqlx::query("SELECT state, version FROM revisions WHERE package_base = ? AND state <> 'superseded' ORDER BY created_at DESC LIMIT 1")
            .bind(base)
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::internal)?;
        let build = sqlx::query("SELECT builds.state, revisions.version FROM builds JOIN revisions ON revisions.id = builds.revision_id WHERE builds.package_base = ? ORDER BY builds.created_at DESC LIMIT 1")
            .bind(base)
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::internal)?;
        items.push(json!({
            "package_base": base,
            "direct": closure.direct.contains(base),
            "required_by": closure.required_by.get(base).cloned().unwrap_or_default(),
            "version": package.as_ref().and_then(|row| row.get::<Option<String>, _>("version")),
            "description": package.as_ref().and_then(|row| row.get::<Option<String>, _>("description")),
            "maintainer": package.as_ref().and_then(|row| row.get::<Option<String>, _>("maintainer")),
            "out_of_date": package.as_ref().and_then(|row| row.get::<Option<i64>, _>("out_of_date")),
            "outputs": package.as_ref().map(|row| serde_json::from_str::<Value>(row.get("outputs_json")).unwrap_or(Value::Null)),
            "sync_error": package.as_ref().and_then(|row| row.get::<Option<String>, _>("sync_error")),
            "last_checked_at": package.as_ref().and_then(|row| row.get::<Option<String>, _>("last_checked_at")),
            "revision_state": revision.as_ref().map(|row| row.get::<String, _>("state")),
            "revision_version": revision.as_ref().map(|row| row.get::<String, _>("version")),
            "build_state": build.as_ref().map(|row| row.get::<String, _>("state")),
            "build_version": build.as_ref().map(|row| row.get::<String, _>("version")),
            "unresolved_providers": closure.unresolved.get(base).cloned().unwrap_or_default(),
        }));
    }
    items.sort_by_key(|item| {
        (
            !item["direct"].as_bool().unwrap_or(false),
            item["package_base"].as_str().unwrap_or_default().to_owned(),
        )
    });
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    package_name: String,
}

pub async fn subscribe(
    State(state): State<AppState>,
    Json(request): Json<SubscribeRequest>,
) -> Result<Json<Value>, ApiError> {
    let name = validate_package_name(request.package_name.trim())
        .map_err(|error| ApiError::bad_request("INVALID_PACKAGE_NAME", error.to_string()))?
        .to_owned();
    let package = state
        .aur
        .info(std::slice::from_ref(&name))
        .await
        .map_err(|error| ApiError::bad_request("AUR_LOOKUP_FAILED", format!("{error:#}")))?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found("AUR 中没有这个软件包"))?;
    let official =
        state.aur.official(&name).await.map_err(|error| {
            ApiError::bad_request("OFFICIAL_LOOKUP_FAILED", format!("{error:#}"))
        })?;
    if official.iter().any(|candidate| candidate.pkgname == name) {
        return Err(ApiError::conflict(
            "PACKAGE_IN_OFFICIAL_REPOSITORY",
            "官方仓库已提供同名软件包，无需订阅",
        ));
    }
    let now = Utc::now();
    let mut transaction = state.db.begin().await.map_err(ApiError::internal)?;
    ensure_package(&mut transaction, &package.package_base, now)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("UPDATE packages SET next_check_at = ? WHERE package_base = ?")
        .bind(now)
        .bind(&package.package_base)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("INSERT OR IGNORE INTO subscriptions(package_base, created_at) VALUES (?, ?)")
        .bind(&package.package_base)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    update_cache(&state.db, &package)
        .await
        .map_err(ApiError::internal)?;
    let result = sync_package(&state, &package.package_base).await;
    let message = result.as_ref().err().map(|error| format!("{error:#}"));
    record_sync(
        &state,
        &package.package_base,
        result
            .as_ref()
            .map(|_| ())
            .map_err(|error| format!("{error:#}")),
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "package_base": package.package_base,
        "revision": result.ok().flatten().map(|(id, state)| json!({"id": id, "state": state})),
        "sync_error": message,
    })))
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let before = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let deleted = sqlx::query("DELETE FROM subscriptions WHERE package_base = ?")
        .bind(&package_base)
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("没有这个直接订阅"));
    }
    let after = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    state.wake();
    Ok(Json(json!({
        "package_base": package_base,
        "removed_package_bases": before.members.difference(&after.members).collect::<Vec<_>>(),
    })))
}

pub async fn detail(
    State(state): State<AppState>,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let package = sqlx::query("SELECT * FROM packages WHERE package_base = ?")
        .bind(&package_base)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("没有这个软件包"))?;
    let closure = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let revisions = sqlx::query("SELECT id, aur_commit, vcs_commit, version, state, baseline_revision_id, created_at, decided_at FROM revisions WHERE package_base = ? ORDER BY created_at DESC LIMIT 20")
        .bind(&package_base)
        .fetch_all(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let choices: BTreeMap<String, String> = sqlx::query(
        "SELECT dependency, provider_base FROM provider_choices WHERE package_base = ?",
    )
    .bind(&package_base)
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::internal)?
    .into_iter()
    .map(|row| (row.get("dependency"), row.get("provider_base")))
    .collect();
    let dependencies = dependency_revision(&state.db, &package_base)
        .await
        .map_err(ApiError::internal)?
        .map(|(_, dependencies)| dependencies)
        .unwrap_or_default()
        .into_iter()
        .map(|dependency| {
            let selected = choices.get(&dependency.name).cloned();
            json!({
                "name": dependency.name,
                "kind": dependency.kind,
                "state": dependency.state,
                "target": dependency.target.or(selected.clone()),
                "selected": selected,
                "candidates": dependency.candidates,
            })
        })
        .collect::<Vec<_>>();
    let builds = crate::builds::recent_builds(&state.db, Some(&package_base), 10).await?;
    Ok(Json(json!({
        "package_base": package_base,
        "direct": closure.direct.contains(&package_base),
        "in_closure": closure.members.contains(&package_base),
        "required_by": closure.required_by.get(&package_base).cloned().unwrap_or_default(),
        "version": package.get::<Option<String>, _>("version"),
        "description": package.get::<Option<String>, _>("description"),
        "maintainer": package.get::<Option<String>, _>("maintainer"),
        "outputs": serde_json::from_str::<Value>(package.get("outputs_json")).unwrap_or(Value::Null),
        "allow_check": package.get::<i64, _>("allow_check") == 1,
        "sync": {
            "last_checked_at": package.get::<Option<String>, _>("last_checked_at"),
            "next_check_at": package.get::<String, _>("next_check_at"),
            "error": package.get::<Option<String>, _>("sync_error"),
        },
        "revisions": revisions.iter().map(|row| json!({
            "id": row.get::<String, _>("id"),
            "aur_commit": row.get::<String, _>("aur_commit"),
            "vcs_commit": row.get::<Option<String>, _>("vcs_commit"),
            "version": row.get::<String, _>("version"),
            "state": row.get::<String, _>("state"),
            "first_time": row.get::<Option<String>, _>("baseline_revision_id").is_none(),
            "created_at": row.get::<String, _>("created_at"),
            "decided_at": row.get::<Option<String>, _>("decided_at"),
        })).collect::<Vec<_>>(),
        "dependencies": dependencies,
        "builds": builds,
    })))
}

pub async fn refresh(
    State(state): State<AppState>,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let exists: Option<String> =
        sqlx::query_scalar("SELECT package_base FROM packages WHERE package_base = ?")
            .bind(&package_base)
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::internal)?;
    if exists.is_none() {
        return Err(ApiError::not_found("没有这个软件包"));
    }
    let result = sync_package(&state, &package_base).await;
    let message = result.as_ref().err().map(|error| format!("{error:#}"));
    record_sync(
        &state,
        &package_base,
        result
            .as_ref()
            .map(|_| ())
            .map_err(|error| format!("{error:#}")),
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "package_base": package_base,
        "revision": result.ok().flatten().map(|(id, state)| json!({"id": id, "state": state})),
        "sync_error": message,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPolicyRequest {
    allow_check: bool,
}

pub async fn set_build_policy(
    State(state): State<AppState>,
    Path(package_base): Path<String>,
    Json(request): Json<BuildPolicyRequest>,
) -> Result<Json<Value>, ApiError> {
    let updated = sqlx::query("UPDATE packages SET allow_check = ? WHERE package_base = ?")
        .bind(i64::from(request.allow_check))
        .bind(&package_base)
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("没有这个软件包"));
    }
    Ok(Json(
        json!({"package_base": package_base, "allow_check": request.allow_check}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequest {
    selected_package_base: String,
}

pub async fn select_provider(
    State(state): State<AppState>,
    Path((package_base, dependency)): Path<(String, String)>,
    Json(request): Json<ProviderRequest>,
) -> Result<Json<Value>, ApiError> {
    let (_, dependencies) = dependency_revision(&state.db, &package_base)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("该软件包还没有可用 revision"))?;
    let candidate = dependencies
        .iter()
        .find(|item| item.name == dependency && item.state == "needs_selection")
        .ok_or_else(|| {
            ApiError::bad_request("PROVIDER_NOT_REQUIRED", "该依赖不需要选择 provider")
        })?;
    if !candidate
        .candidates
        .contains(&request.selected_package_base)
    {
        return Err(ApiError::bad_request(
            "PROVIDER_NOT_CANDIDATE",
            "所选 pkgbase 不是该依赖的候选 provider",
        ));
    }
    let now = Utc::now();
    let mut transaction = state.db.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO provider_choices(package_base, dependency, provider_base, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(package_base, dependency) DO UPDATE SET provider_base = excluded.provider_base, updated_at = excluded.updated_at")
        .bind(&package_base)
        .bind(&dependency)
        .bind(&request.selected_package_base)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    ensure_package(&mut transaction, &request.selected_package_base, now)
        .await
        .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    state.wake();
    Ok(Json(json!({
        "package_base": package_base,
        "dependency": dependency,
        "selected_package_base": request.selected_package_base,
    })))
}
