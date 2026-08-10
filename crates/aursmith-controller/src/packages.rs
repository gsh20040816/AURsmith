use crate::{
    auth,
    error::ApiError,
    routes::{AppState, append_event_in_transaction},
    transport,
};
use aursmith_domain::DependencyGraph;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UpstreamPackage {
    name: String,
    package_base: String,
    version: String,
    description: Option<String>,
    maintainer: Option<String>,
    out_of_date: Option<i64>,
    last_modified: i64,
    #[serde(default)]
    depends: Vec<String>,
    #[serde(default)]
    make_depends: Vec<String>,
    #[serde(default)]
    check_depends: Vec<String>,
    #[serde(default)]
    opt_depends: Vec<String>,
    #[serde(default)]
    provides: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SnapshotDependency {
    name: String,
    kind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UpstreamSnapshot {
    package_base: String,
    aur_commit: String,
    vcs_commit: Option<String>,
    version: String,
    outputs: Vec<String>,
    dependencies: Vec<SnapshotDependency>,
    optional_dependencies: Vec<String>,
    provides: Vec<String>,
    architectures: Vec<String>,
    sources: Vec<String>,
    srcinfo: String,
}

#[derive(Debug, Clone)]
struct SnapshotNode {
    package: UpstreamPackage,
    snapshot: UpstreamSnapshot,
}

#[derive(Debug, Clone)]
struct DependencyClosure {
    nodes: Vec<SnapshotNode>,
    resolutions: BTreeMap<String, String>,
    provider_candidates: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    package_name: String,
    #[serde(default)]
    followed_outputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SelectProviderRequest {
    selected_package_base: String,
}

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let endpoint = publisher_endpoint(&state.database).await?;
    let reply = transport::aur_search(&state.config, &endpoint, &query.q).await?;
    let packages: Vec<UpstreamPackage> =
        serde_json::from_value(reply.data.get("items").cloned().unwrap_or(Value::Null))
            .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "items": packages.into_iter().map(package_json).collect::<Vec<_>>()
    })))
}

pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubscribeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let administrator_id = auth::require_administrator(&state, &headers).await?;
    validate_name(&request.package_name)?;
    let endpoint = publisher_endpoint(&state.database).await?;
    let official = transport::official_info(
        &state.config,
        &endpoint,
        std::slice::from_ref(&request.package_name),
    )
    .await?;
    if official
        .data
        .get(&request.package_name)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return Err(ApiError::conflict(
            "PACKAGE_IN_OFFICIAL_REPOSITORY",
            "同名软件包已经进入 Arch 官方仓库，请优先使用官方包",
        ));
    }
    let info_reply = transport::aur_info(
        &state.config,
        &endpoint,
        std::slice::from_ref(&request.package_name),
    )
    .await?;
    let packages: Vec<UpstreamPackage> =
        serde_json::from_value(info_reply.data.get("items").cloned().unwrap_or(Value::Null))
            .map_err(ApiError::internal)?;
    let package = packages
        .into_iter()
        .find(|package| package.name == request.package_name)
        .ok_or_else(|| ApiError::not_found("AUR 中不存在该软件包"))?;
    let snapshot_reply =
        transport::aur_snapshot(&state.config, &endpoint, &package.package_base).await?;
    let snapshot: UpstreamSnapshot =
        serde_json::from_value(snapshot_reply.data).map_err(ApiError::internal)?;
    let dependency_closure =
        collect_dependency_snapshots(&state, &endpoint, &snapshot, &BTreeMap::new()).await?;
    let result = apply_snapshot(
        &state.database,
        &administrator_id,
        &package,
        &snapshot,
        &request.followed_outputs,
        &dependency_closure,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn collect_dependency_snapshots(
    state: &AppState,
    endpoint: &str,
    root: &UpstreamSnapshot,
    selected_providers: &BTreeMap<String, String>,
) -> Result<DependencyClosure, ApiError> {
    const MAXIMUM_AUR_DEPENDENCY_BASES: usize = 64;
    let mut snapshots = BTreeMap::<String, UpstreamSnapshot>::new();
    let mut packages = BTreeMap::<String, UpstreamPackage>::new();
    let mut resolutions = BTreeMap::<String, String>::new();
    let mut provider_candidates = BTreeMap::<String, Vec<String>>::new();
    let mut pending = vec![root.clone()];
    while let Some(current) = pending.pop() {
        let names: Vec<String> = current
            .dependencies
            .iter()
            .map(|dependency| dependency.name.clone())
            .filter(|name| name != &root.package_base)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(200)
            .collect();
        if names.is_empty() {
            continue;
        }
        let reply = transport::aur_info(&state.config, endpoint, &names).await?;
        let found: Vec<UpstreamPackage> =
            serde_json::from_value(reply.data.get("items").cloned().unwrap_or(Value::Null))
                .map_err(ApiError::internal)?;
        let found_names: BTreeSet<String> =
            found.iter().map(|package| package.name.clone()).collect();
        let mut discovered = Vec::new();
        for package in found {
            if !names.contains(&package.name) || package.package_base == root.package_base {
                continue;
            }
            resolutions.insert(package.name.clone(), package.package_base.clone());
            discovered.push(package);
        }
        let unresolved: Vec<String> = names
            .iter()
            .filter(|name| !found_names.contains(*name))
            .cloned()
            .collect();
        for chunk in unresolved.chunks(50) {
            if chunk.is_empty() {
                continue;
            }
            let reply = transport::aur_providers(&state.config, endpoint, chunk).await?;
            let values = reply
                .data
                .as_object()
                .ok_or_else(|| ApiError::internal("Provider 响应不是对象"))?;
            for name in chunk {
                let candidates: Vec<UpstreamPackage> =
                    serde_json::from_value(values.get(name).cloned().unwrap_or_else(|| json!([])))
                        .map_err(ApiError::internal)?;
                let bases: BTreeSet<String> = candidates
                    .iter()
                    .map(|candidate| candidate.package_base.clone())
                    .filter(|base| base != &root.package_base)
                    .collect();
                let selected = selected_providers
                    .get(name)
                    .filter(|selected| bases.contains(*selected))
                    .cloned();
                if bases.len() == 1 || selected.is_some() {
                    let package_base = selected
                        .unwrap_or_else(|| bases.iter().next().expect("长度已经检查").clone());
                    resolutions.insert(name.clone(), package_base.clone());
                    if let Some(candidate) = candidates
                        .into_iter()
                        .find(|candidate| candidate.package_base == package_base)
                    {
                        discovered.push(candidate);
                    }
                } else if bases.len() > 1 {
                    provider_candidates.insert(name.clone(), bases.into_iter().collect());
                }
            }
        }
        for package in discovered {
            packages
                .entry(package.package_base.clone())
                .or_insert_with(|| package.clone());
            if snapshots.contains_key(&package.package_base) {
                continue;
            }
            if snapshots.len() >= MAXIMUM_AUR_DEPENDENCY_BASES {
                return Err(ApiError::conflict(
                    "DEPENDENCY_GRAPH_TOO_LARGE",
                    "AUR 依赖闭包超过第一版 64 个 pkgbase 的安全上限",
                ));
            }
            let reply =
                transport::aur_snapshot(&state.config, endpoint, &package.package_base).await?;
            let snapshot: UpstreamSnapshot =
                serde_json::from_value(reply.data).map_err(ApiError::internal)?;
            if snapshot.package_base != package.package_base {
                return Err(ApiError::conflict(
                    "UPSTREAM_MISMATCH",
                    "依赖的 AUR RPC 与 Git 快照不一致",
                ));
            }
            pending.push(snapshot.clone());
            snapshots.insert(package.package_base.clone(), snapshot);
        }
    }
    let nodes = snapshots
        .into_iter()
        .filter_map(|(package_base, snapshot)| {
            packages
                .get(&package_base)
                .cloned()
                .map(|package| SnapshotNode { package, snapshot })
        })
        .collect();
    Ok(DependencyClosure {
        nodes,
        resolutions,
        provider_candidates,
    })
}

async fn apply_snapshot(
    database: &SqlitePool,
    actor: &str,
    package: &UpstreamPackage,
    snapshot: &UpstreamSnapshot,
    requested_outputs: &[String],
    dependency_closure: &DependencyClosure,
) -> Result<Value, ApiError> {
    if package.package_base != snapshot.package_base || snapshot.outputs.is_empty() {
        return Err(ApiError::conflict(
            "UPSTREAM_MISMATCH",
            "AUR RPC 与 Git 快照的 pkgbase 不一致",
        ));
    }
    let outputs: BTreeSet<_> = snapshot.outputs.iter().cloned().collect();
    let followed_outputs: Vec<String> = if requested_outputs.is_empty() {
        snapshot.outputs.clone()
    } else {
        let requested: BTreeSet<_> = requested_outputs.iter().cloned().collect();
        if !requested.is_subset(&outputs) {
            return Err(ApiError::bad_request(
                "INVALID_SPLIT_OUTPUT",
                "关注的 split output 不属于该 pkgbase",
            ));
        }
        requested.into_iter().collect()
    };
    let dependency_map = &dependency_closure.resolutions;
    let metadata = serde_json::to_value(snapshot).map_err(ApiError::internal)?;
    let provider_selection_sha256 = selection_digest(snapshot, dependency_map)?;
    let input_sha256 = revision_input_digest(snapshot, dependency_map)?;
    let now = Utc::now();
    let mut transaction = database.begin().await.map_err(ApiError::internal)?;

    sqlx::query(
        "INSERT INTO package_bases(name, version, description, maintainer, out_of_date_at, orphaned, vcs_kind, outputs_json, dependencies_json, optional_dependencies_json, provides_json, architectures_json, aur_last_modified, last_synced_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(name) DO UPDATE SET version = excluded.version, description = excluded.description, maintainer = excluded.maintainer, out_of_date_at = excluded.out_of_date_at, orphaned = excluded.orphaned, vcs_kind = excluded.vcs_kind, outputs_json = excluded.outputs_json, dependencies_json = excluded.dependencies_json, optional_dependencies_json = excluded.optional_dependencies_json, provides_json = excluded.provides_json, architectures_json = excluded.architectures_json, aur_last_modified = excluded.aur_last_modified, last_synced_at = excluded.last_synced_at",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.version)
    .bind(&package.description)
    .bind(&package.maintainer)
    .bind(package.out_of_date)
    .bind(i64::from(package.maintainer.is_none()))
    .bind(vcs_kind(&snapshot.package_base))
    .bind(json_string(&snapshot.outputs)?)
    .bind(json_string(&snapshot.dependencies)?)
    .bind(json_string(&snapshot.optional_dependencies)?)
    .bind(json_string(&snapshot.provides)?)
    .bind(json_string(&snapshot.architectures)?)
    .bind(package.last_modified)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;

    let subscription_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO subscriptions(id, package_base, kind, state, reference_count, followed_outputs_json, selected_providers_json, created_at, updated_at) VALUES (?, ?, 'direct', 'active', 0, ?, '{}', ?, ?) ON CONFLICT(package_base, kind) DO UPDATE SET state = 'active', followed_outputs_json = excluded.followed_outputs_json, updated_at = excluded.updated_at",
    )
    .bind(&subscription_id)
    .bind(&snapshot.package_base)
    .bind(json_string(&followed_outputs)?)
    .bind(now)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;

    sqlx::query("UPDATE revisions SET state = 'superseded' WHERE package_base = ? AND aur_commit != ? AND state IN ('discovered', 'fetching', 'audit_pending', 'build_pending')")
        .bind(&snapshot.package_base)
        .bind(&snapshot.aur_commit)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    supersede_other_revisions(&mut transaction, snapshot, &provider_selection_sha256).await?;
    let existing_revision: Option<String> = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND aur_commit = ? AND COALESCE(vcs_commit, '') = COALESCE(?, '') AND audit_policy_version = 'v1' AND provider_selection_sha256 = ?",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .bind(&provider_selection_sha256)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    let idempotent_revision = existing_revision.is_some();
    let revision_id = existing_revision.unwrap_or_else(|| Uuid::new_v4().to_string());
    sqlx::query(
        "INSERT OR IGNORE INTO revisions(id, package_base, aur_commit, vcs_commit, upstream_version, input_sha256, audit_policy_version, provider_selection_sha256, state, metadata_json, created_at) VALUES (?, ?, ?, ?, ?, ?, 'v1', ?, 'discovered', ?, ?)",
    )
    .bind(&revision_id)
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .bind(&snapshot.version)
    .bind(&input_sha256)
    .bind(&provider_selection_sha256)
    .bind(metadata.to_string())
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;

    sqlx::query("DELETE FROM subscription_references WHERE owner_package_base = ?")
        .bind(&snapshot.package_base)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    for dependency in &snapshot.dependencies {
        let target = dependency_map.get(&dependency.name).cloned();
        let candidates = dependency_closure
            .provider_candidates
            .get(&dependency.name)
            .cloned()
            .unwrap_or_default();
        let provider_state = if target.is_some() {
            "resolved"
        } else if !candidates.is_empty() {
            "needs_selection"
        } else {
            "official_or_unknown"
        };
        sqlx::query(
            "INSERT OR REPLACE INTO revision_dependencies(revision_id, dependency_name, dependency_kind, target_package_base, provider_state, candidates_json) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&revision_id)
        .bind(&dependency.name)
        .bind(&dependency.kind)
        .bind(&target)
        .bind(provider_state)
        .bind(if candidates.is_empty() {
            target
                .as_ref()
                .map(|value| json!([value]))
                .unwrap_or_else(|| json!([]))
        } else {
            json!(candidates)
        }
        .to_string())
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        if let Some(target) = target.filter(|target| target != &snapshot.package_base) {
            sqlx::query("INSERT OR IGNORE INTO subscription_references(owner_package_base, dependency_package_base, created_at) VALUES (?, ?, ?)")
                .bind(&snapshot.package_base)
                .bind(&target)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(ApiError::internal)?;
            sqlx::query(
                "INSERT INTO subscriptions(id, package_base, kind, state, reference_count, followed_outputs_json, selected_providers_json, created_at, updated_at) VALUES (?, ?, 'implicit', 'active', 1, '[]', '{}', ?, ?) ON CONFLICT(package_base, kind) DO UPDATE SET state = 'active', updated_at = excluded.updated_at",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&target)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?;
        }
    }
    for node in &dependency_closure.nodes {
        upsert_implicit_node(
            &mut transaction,
            node,
            dependency_map,
            &dependency_closure.provider_candidates,
            now,
        )
        .await?;
    }
    recalculate_reference_counts(&mut transaction).await?;

    let graph = load_dependency_graph(&mut transaction).await?;
    let (batch_id, batch_state) = if idempotent_revision {
        (None, "unchanged")
    } else {
        let batch_id = Uuid::new_v4().to_string();
        let (batch_state, failure_reason) = match graph.topological_order() {
            Err(error) => ("blocked_cycle", Some(error.to_string())),
            Ok(_) if !dependency_closure.provider_candidates.is_empty() => (
                "awaiting_provider_selection",
                Some("一个或多个虚拟依赖存在多个 Provider 候选".to_owned()),
            ),
            Ok(_) => ("awaiting_audit", None),
        };
        sqlx::query(
            "INSERT INTO release_batches(id, state, graph_json, failure_reason, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&batch_id)
        .bind(batch_state)
        .bind(serde_json::to_string(&graph).map_err(ApiError::internal)?)
        .bind(&failure_reason)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
        (Some(batch_id), batch_state)
    };
    append_event_in_transaction(
        &mut transaction,
        "subscription",
        &snapshot.package_base,
        "package_subscribed",
        json!({"revision_id": revision_id, "batch_id": batch_id, "aur_commit": snapshot.aur_commit, "idempotent": idempotent_revision}),
        actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(json!({
        "package_base": snapshot.package_base,
        "revision_id": revision_id,
        "batch_id": batch_id,
        "batch_state": batch_state,
        "idempotent": idempotent_revision
    }))
}

pub async fn list_subscriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT subscriptions.id, subscriptions.package_base, subscriptions.kind, subscriptions.state, subscriptions.reference_count, subscriptions.followed_outputs_json, package_bases.version, package_bases.description, package_bases.outputs_json, package_bases.maintainer, package_bases.out_of_date_at FROM subscriptions LEFT JOIN package_bases ON package_bases.name = subscriptions.package_base WHERE subscriptions.state != 'purged' ORDER BY subscriptions.kind, subscriptions.package_base",
    )
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let items: Result<Vec<_>, ApiError> = rows.into_iter().map(subscription_json).collect();
    Ok(Json(json!({"items": items?})))
}

pub async fn package_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    validate_name(&package_base)?;
    let package = sqlx::query("SELECT * FROM package_bases WHERE name = ?")
        .bind(&package_base)
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("软件包尚未同步"))?;
    let revisions = sqlx::query("SELECT id, aur_commit, vcs_commit, upstream_version, published_version, input_sha256, state, created_at FROM revisions WHERE package_base = ? ORDER BY created_at DESC")
        .bind(&package_base)
        .fetch_all(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let blockers = sqlx::query("SELECT revision_dependencies.dependency_name, revision_dependencies.dependency_kind, revision_dependencies.target_package_base, revision_dependencies.provider_state, revision_dependencies.candidates_json FROM revision_dependencies JOIN revisions ON revisions.id = revision_dependencies.revision_id WHERE revisions.package_base = ? AND revisions.state != 'superseded' ORDER BY revision_dependencies.dependency_kind, revision_dependencies.dependency_name")
        .bind(&package_base)
        .fetch_all(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "package_base": package.get::<String, _>("name"),
        "version": package.get::<String, _>("version"),
        "description": package.get::<Option<String>, _>("description"),
        "maintainer": package.get::<Option<String>, _>("maintainer"),
        "outputs": parse_json::<Vec<String>>(package.get("outputs_json"))?,
        "dependencies": parse_json::<Value>(package.get("dependencies_json"))?,
        "optional_dependencies": parse_json::<Value>(package.get("optional_dependencies_json"))?,
        "provides": parse_json::<Value>(package.get("provides_json"))?,
        "architectures": parse_json::<Value>(package.get("architectures_json"))?,
        "revisions": revisions.into_iter().map(|row| json!({
            "id": row.get::<String, _>("id"), "aur_commit": row.get::<String, _>("aur_commit"),
            "vcs_commit": row.get::<Option<String>, _>("vcs_commit"), "upstream_version": row.get::<String, _>("upstream_version"),
            "published_version": row.get::<Option<String>, _>("published_version"), "input_sha256": row.get::<String, _>("input_sha256"),
            "state": row.get::<String, _>("state"), "created_at": row.get::<String, _>("created_at")
        })).collect::<Vec<_>>(),
        "dependency_resolution": blockers.into_iter().map(|row| json!({
            "name": row.get::<String, _>("dependency_name"), "kind": row.get::<String, _>("dependency_kind"),
            "target_package_base": row.get::<Option<String>, _>("target_package_base"), "state": row.get::<String, _>("provider_state"),
            "candidates": parse_json::<Value>(row.get("candidates_json")).unwrap_or_else(|_| json!([]))
        })).collect::<Vec<_>>()
    })))
}

pub async fn pause(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    change_direct_state(
        &state,
        &headers,
        &package_base,
        "active",
        "paused",
        "subscription_paused",
    )
    .await
}

pub async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    change_direct_state(
        &state,
        &headers,
        &package_base,
        "paused",
        "active",
        "subscription_resumed",
    )
    .await
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    validate_name(&package_base)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    let result =
        sqlx::query("DELETE FROM subscriptions WHERE package_base = ? AND kind = 'direct'")
            .bind(&package_base)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("不存在直接订阅"));
    }
    sqlx::query("DELETE FROM subscription_references WHERE owner_package_base = ?")
        .bind(&package_base)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    recalculate_reference_counts(&mut transaction).await?;
    append_event_in_transaction(
        &mut transaction,
        "subscription",
        &package_base,
        "subscription_removed",
        json!({}),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(Json(
        json!({"package_base": package_base, "direct_subscription": false}),
    ))
}

pub async fn purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    validate_name(&package_base)?;
    let references: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscription_references WHERE dependency_package_base = ?",
    )
    .bind(&package_base)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if references > 0 {
        return Err(ApiError::conflict(
            "PACKAGE_STILL_REQUIRED",
            format!("仍有 {references} 个订阅依赖该软件包"),
        ));
    }
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE subscriptions SET state = 'purged', updated_at = ? WHERE package_base = ?")
        .bind(Utc::now())
        .bind(&package_base)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    let batch_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO release_batches(id, state, graph_json, failure_reason, created_at, updated_at) VALUES (?, 'queued_removal', ?, NULL, ?, ?)")
        .bind(&batch_id).bind(json!({"remove": [&package_base]}).to_string()).bind(Utc::now()).bind(Utc::now())
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    append_event_in_transaction(
        &mut transaction,
        "subscription",
        &package_base,
        "package_purge_requested",
        json!({"batch_id": batch_id}),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(Json(
        json!({"package_base": package_base, "batch_id": batch_id, "state": "queued_removal"}),
    ))
}

async fn change_direct_state(
    state: &AppState,
    headers: &HeaderMap,
    package_base: &str,
    from: &str,
    to: &str,
    event: &str,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(state, headers).await?;
    validate_name(package_base)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    let result = sqlx::query("UPDATE subscriptions SET state = ?, updated_at = ? WHERE package_base = ? AND kind = 'direct' AND state = ?")
        .bind(to).bind(Utc::now()).bind(package_base).bind(from).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "INVALID_SUBSCRIPTION_STATE",
            "订阅当前状态不允许此操作",
        ));
    }
    append_event_in_transaction(
        &mut transaction,
        "subscription",
        package_base,
        event,
        json!({"state": to}),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(Json(json!({"package_base": package_base, "state": to})))
}

pub async fn list_batches(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT id, state, graph_json, current_release_id, failure_reason, created_at, updated_at FROM release_batches ORDER BY created_at DESC LIMIT 200")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "id": row.get::<String, _>("id"), "state": row.get::<String, _>("state"),
        "graph": parse_json::<Value>(row.get("graph_json")).unwrap_or(Value::Null),
        "current_release_id": row.get::<Option<String>, _>("current_release_id"),
        "failure_reason": row.get::<Option<String>, _>("failure_reason"),
        "created_at": row.get::<String, _>("created_at"), "updated_at": row.get::<String, _>("updated_at")
    })).collect::<Vec<_>>() })))
}

pub async fn refresh_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    validate_name(&package_base)?;
    let result = refresh_one(&state, &package_base, &actor).await?;
    Ok(Json(result))
}

pub async fn select_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((package_base, dependency_name)): Path<(String, String)>,
    Json(request): Json<SelectProviderRequest>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    validate_name(&package_base)?;
    validate_name(&dependency_name)?;
    validate_name(&request.selected_package_base)?;
    let candidates_json: String = sqlx::query_scalar(
        "SELECT revision_dependencies.candidates_json FROM revision_dependencies JOIN revisions ON revisions.id = revision_dependencies.revision_id WHERE revisions.package_base = ? AND revisions.state != 'superseded' AND revision_dependencies.dependency_name = ? AND revision_dependencies.provider_state = 'needs_selection' ORDER BY revisions.created_at DESC LIMIT 1",
    )
    .bind(&package_base)
    .bind(&dependency_name)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::conflict("PROVIDER_NOT_REQUIRED", "该依赖当前不需要选择 Provider"))?;
    let candidates: Vec<String> = parse_json(&candidates_json)?;
    if !candidates.contains(&request.selected_package_base) {
        return Err(ApiError::bad_request(
            "INVALID_PROVIDER",
            "选择项不在当前 Revision 的 Provider 候选中",
        ));
    }
    let selected_json: String = sqlx::query_scalar("SELECT selected_providers_json FROM subscriptions WHERE package_base = ? AND kind = 'direct'")
        .bind(&package_base).fetch_one(&state.database).await.map_err(ApiError::internal)?;
    let mut selected: BTreeMap<String, String> = parse_json(&selected_json)?;
    selected.insert(
        dependency_name.clone(),
        request.selected_package_base.clone(),
    );
    sqlx::query("UPDATE subscriptions SET selected_providers_json = ?, updated_at = ? WHERE package_base = ? AND kind = 'direct'")
        .bind(json_string(&selected)?).bind(Utc::now()).bind(&package_base)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    let result = refresh_one(&state, &package_base, &actor).await?;
    Ok(Json(json!({
        "package_base": package_base,
        "dependency_name": dependency_name,
        "selected_package_base": request.selected_package_base,
        "refresh": result
    })))
}

pub async fn refresh_due(state: &AppState) -> Result<(), ApiError> {
    let rows = sqlx::query(
        "SELECT subscriptions.package_base FROM subscriptions LEFT JOIN package_sync_state ON package_sync_state.package_base = subscriptions.package_base WHERE subscriptions.kind = 'direct' AND subscriptions.state = 'active' AND (package_sync_state.next_check_at IS NULL OR package_sync_state.next_check_at <= ?) ORDER BY subscriptions.package_base LIMIT 10",
    )
    .bind(Utc::now())
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    for row in rows {
        let package_base: String = row.get("package_base");
        if let Err(error) = refresh_one(state, &package_base, "scheduler").await {
            record_sync_failure(state, &package_base, &error.to_string()).await?;
            tracing::warn!(%package_base, %error, "AUR 定时同步失败");
        }
    }
    Ok(())
}

async fn refresh_one(state: &AppState, package_base: &str, actor: &str) -> Result<Value, ApiError> {
    let row = sqlx::query("SELECT package_bases.outputs_json, package_bases.vcs_kind, subscriptions.followed_outputs_json, subscriptions.selected_providers_json, package_sync_state.last_official_checked_at FROM package_bases JOIN subscriptions ON subscriptions.package_base = package_bases.name AND subscriptions.kind = 'direct' LEFT JOIN package_sync_state ON package_sync_state.package_base = package_bases.name WHERE package_bases.name = ?")
        .bind(package_base)
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("不存在可同步的直接订阅"))?;
    let outputs: Vec<String> = parse_json(row.get("outputs_json"))?;
    let followed_outputs: Vec<String> = parse_json(row.get("followed_outputs_json"))?;
    let selected_providers: BTreeMap<String, String> =
        parse_json(row.get("selected_providers_json"))?;
    let endpoint = publisher_endpoint(&state.database).await?;
    let last_official_check: Option<chrono::DateTime<Utc>> = row
        .get::<Option<String>, _>("last_official_checked_at")
        .and_then(|value| value.parse().ok());
    let official_check_due = last_official_check
        .is_none_or(|checked| checked <= Utc::now() - chrono::Duration::hours(6));
    if official_check_due {
        let official = transport::official_info(&state.config, &endpoint, &outputs).await?;
        let now = Utc::now();
        sqlx::query("INSERT INTO package_sync_state(package_base, last_official_checked_at) VALUES (?, ?) ON CONFLICT(package_base) DO UPDATE SET last_official_checked_at = excluded.last_official_checked_at")
            .bind(package_base).bind(now).execute(&state.database).await.map_err(ApiError::internal)?;
        if official.data.as_object().is_some_and(|items| {
            items.values().any(|value| {
                value
                    .as_array()
                    .is_some_and(|packages| !packages.is_empty())
            })
        }) {
            sqlx::query("UPDATE subscriptions SET state = 'paused', updated_at = ? WHERE package_base = ? AND kind = 'direct'")
                .bind(now).bind(package_base).execute(&state.database).await.map_err(ApiError::internal)?;
            sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'info', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = 'open', details_json = excluded.details_json, resolved_at = NULL")
                .bind(Uuid::new_v4().to_string()).bind(format!("official-promotion:{package_base}"))
                .bind(format!("软件包已进入 Arch 官方仓库：{package_base}"))
                .bind(json!({"package_base": package_base, "outputs": outputs}).to_string()).bind(now)
                .execute(&state.database).await.map_err(ApiError::internal)?;
            append_event(
                &state.database,
                "package_base",
                package_base,
                "package_promoted_to_official",
                json!({"outputs": outputs}),
                actor,
            )
            .await?;
            return Ok(
                json!({"package_base": package_base, "state": "official_migration_required"}),
            );
        }
    }
    let reply = transport::aur_info(&state.config, &endpoint, &outputs).await?;
    let packages: Vec<UpstreamPackage> =
        serde_json::from_value(reply.data.get("items").cloned().unwrap_or(Value::Null))
            .map_err(ApiError::internal)?;
    let package = packages
        .into_iter()
        .find(|package| package.package_base == package_base)
        .ok_or_else(|| {
            ApiError::conflict("AUR_PACKAGE_MISSING", "AUR 软件包可能已删除、重命名或合并")
        })?;
    let snapshot_reply = transport::aur_snapshot(&state.config, &endpoint, package_base).await?;
    let snapshot: UpstreamSnapshot =
        serde_json::from_value(snapshot_reply.data).map_err(ApiError::internal)?;
    let closure =
        collect_dependency_snapshots(state, &endpoint, &snapshot, &selected_providers).await?;
    let result = apply_snapshot(
        &state.database,
        actor,
        &package,
        &snapshot,
        &followed_outputs,
        &closure,
    )
    .await?;
    let interval_hours = if row.get::<Option<String>, _>("vcs_kind").as_deref() == Some("git") {
        24
    } else {
        0
    };
    let next = if interval_hours == 0 {
        Utc::now() + chrono::Duration::minutes(30)
    } else {
        Utc::now() + chrono::Duration::hours(interval_hours)
    };
    sqlx::query("INSERT INTO package_sync_state(package_base, consecutive_failures, last_checked_at, last_success_at, last_error, next_check_at) VALUES (?, 0, ?, ?, NULL, ?) ON CONFLICT(package_base) DO UPDATE SET consecutive_failures = 0, last_checked_at = excluded.last_checked_at, last_success_at = excluded.last_success_at, last_error = NULL, next_check_at = excluded.next_check_at")
        .bind(package_base).bind(Utc::now()).bind(Utc::now()).bind(next)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE alerts SET state = 'resolved', resolved_at = ? WHERE fingerprint = ? AND state != 'resolved'")
        .bind(Utc::now()).bind(format!("aur-sync:{package_base}"))
        .execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(result)
}

async fn record_sync_failure(
    state: &AppState,
    package_base: &str,
    error: &str,
) -> Result<(), ApiError> {
    let next = Utc::now() + chrono::Duration::minutes(30);
    sqlx::query("INSERT INTO package_sync_state(package_base, consecutive_failures, last_checked_at, last_error, next_check_at) VALUES (?, 1, ?, ?, ?) ON CONFLICT(package_base) DO UPDATE SET consecutive_failures = package_sync_state.consecutive_failures + 1, last_checked_at = excluded.last_checked_at, last_error = excluded.last_error, next_check_at = excluded.next_check_at")
        .bind(package_base).bind(Utc::now()).bind(error).bind(next)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    let failures: i64 = sqlx::query_scalar(
        "SELECT consecutive_failures FROM package_sync_state WHERE package_base = ?",
    )
    .bind(package_base)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if failures >= 3 {
        sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = 'open', details_json = excluded.details_json, resolved_at = NULL")
            .bind(Uuid::new_v4().to_string()).bind(format!("aur-sync:{package_base}"))
            .bind(format!("AUR 连续同步失败：{package_base}"))
            .bind(json!({"package_base": package_base, "consecutive_failures": failures, "last_error": error}).to_string())
            .bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    }
    Ok(())
}

async fn publisher_endpoint(database: &SqlitePool) -> Result<String, ApiError> {
    sqlx::query_scalar(
        "SELECT endpoint FROM workers WHERE role = 'publisher' AND state = 'online' LIMIT 1",
    )
    .fetch_optional(database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::conflict("NO_ELIGIBLE_PUBLISHER", "没有在线 Publisher，无法访问 AUR"))
}

async fn append_event(
    database: &SqlitePool,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    payload: Value,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO events(event_id, aggregate_type, aggregate_id, event_type, payload_json, actor, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(aggregate_type).bind(aggregate_id)
        .bind(event_type).bind(payload.to_string()).bind(actor).bind(Utc::now())
        .execute(database).await.map_err(ApiError::internal)?;
    Ok(())
}

async fn recalculate_reference_counts(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE subscriptions SET reference_count = (SELECT COUNT(*) FROM subscription_references WHERE dependency_package_base = subscriptions.package_base), state = CASE WHEN kind = 'implicit' AND (SELECT COUNT(*) FROM subscription_references WHERE dependency_package_base = subscriptions.package_base) = 0 THEN 'retained_without_references' WHEN kind = 'implicit' THEN 'active' ELSE state END, updated_at = ? WHERE kind = 'implicit'")
        .bind(Utc::now()).execute(&mut **transaction).await.map_err(ApiError::internal)?;
    Ok(())
}

async fn supersede_other_revisions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    snapshot: &UpstreamSnapshot,
    provider_selection_sha256: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE revisions SET state = 'superseded' WHERE package_base = ? AND state IN ('discovered', 'fetching', 'audit_pending', 'build_pending') AND (aur_commit != ? OR COALESCE(vcs_commit, '') != COALESCE(?, '') OR provider_selection_sha256 != ?)")
        .bind(&snapshot.package_base)
        .bind(&snapshot.aur_commit)
        .bind(&snapshot.vcs_commit)
        .bind(provider_selection_sha256)
        .execute(&mut **transaction)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

async fn upsert_implicit_node(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    node: &SnapshotNode,
    dependency_map: &BTreeMap<String, String>,
    provider_candidates: &BTreeMap<String, Vec<String>>,
    now: chrono::DateTime<Utc>,
) -> Result<(), ApiError> {
    let snapshot = &node.snapshot;
    let package = &node.package;
    let metadata = serde_json::to_value(snapshot).map_err(ApiError::internal)?;
    let provider_selection_sha256 = selection_digest(snapshot, dependency_map)?;
    let input_sha256 = revision_input_digest(snapshot, dependency_map)?;
    sqlx::query(
        "INSERT INTO package_bases(name, version, description, maintainer, out_of_date_at, orphaned, vcs_kind, outputs_json, dependencies_json, optional_dependencies_json, provides_json, architectures_json, aur_last_modified, last_synced_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(name) DO UPDATE SET version = excluded.version, description = excluded.description, maintainer = excluded.maintainer, out_of_date_at = excluded.out_of_date_at, orphaned = excluded.orphaned, vcs_kind = excluded.vcs_kind, outputs_json = excluded.outputs_json, dependencies_json = excluded.dependencies_json, optional_dependencies_json = excluded.optional_dependencies_json, provides_json = excluded.provides_json, architectures_json = excluded.architectures_json, aur_last_modified = excluded.aur_last_modified, last_synced_at = excluded.last_synced_at",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.version)
    .bind(&package.description)
    .bind(&package.maintainer)
    .bind(package.out_of_date)
    .bind(i64::from(package.maintainer.is_none()))
    .bind(vcs_kind(&snapshot.package_base))
    .bind(json_string(&snapshot.outputs)?)
    .bind(json_string(&snapshot.dependencies)?)
    .bind(json_string(&snapshot.optional_dependencies)?)
    .bind(json_string(&snapshot.provides)?)
    .bind(json_string(&snapshot.architectures)?)
    .bind(package.last_modified)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO subscriptions(id, package_base, kind, state, reference_count, followed_outputs_json, selected_providers_json, created_at, updated_at) VALUES (?, ?, 'implicit', 'active', 0, '[]', '{}', ?, ?) ON CONFLICT(package_base, kind) DO UPDATE SET state = 'active', updated_at = excluded.updated_at",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&snapshot.package_base)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(ApiError::internal)?;
    sqlx::query("UPDATE revisions SET state = 'superseded' WHERE package_base = ? AND aur_commit != ? AND state IN ('discovered', 'fetching', 'audit_pending', 'build_pending')")
        .bind(&snapshot.package_base)
        .bind(&snapshot.aur_commit)
        .execute(&mut **transaction)
        .await
        .map_err(ApiError::internal)?;
    supersede_other_revisions(transaction, snapshot, &provider_selection_sha256).await?;
    let revision_id: String = sqlx::query_scalar(
        "SELECT id FROM revisions WHERE package_base = ? AND aur_commit = ? AND COALESCE(vcs_commit, '') = COALESCE(?, '') AND audit_policy_version = 'v1' AND provider_selection_sha256 = ?",
    )
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .bind(&provider_selection_sha256)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(ApiError::internal)?
    .unwrap_or_else(|| Uuid::new_v4().to_string());
    sqlx::query(
        "INSERT OR IGNORE INTO revisions(id, package_base, aur_commit, vcs_commit, upstream_version, input_sha256, audit_policy_version, provider_selection_sha256, state, metadata_json, created_at) VALUES (?, ?, ?, ?, ?, ?, 'v1', ?, 'discovered', ?, ?)",
    )
    .bind(&revision_id)
    .bind(&snapshot.package_base)
    .bind(&snapshot.aur_commit)
    .bind(&snapshot.vcs_commit)
    .bind(&snapshot.version)
    .bind(input_sha256)
    .bind(provider_selection_sha256)
    .bind(metadata.to_string())
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(ApiError::internal)?;
    sqlx::query("DELETE FROM subscription_references WHERE owner_package_base = ?")
        .bind(&snapshot.package_base)
        .execute(&mut **transaction)
        .await
        .map_err(ApiError::internal)?;
    for dependency in &snapshot.dependencies {
        let target = dependency_map.get(&dependency.name).cloned();
        let candidates = provider_candidates
            .get(&dependency.name)
            .cloned()
            .unwrap_or_default();
        let provider_state = if target.is_some() {
            "resolved"
        } else if !candidates.is_empty() {
            "needs_selection"
        } else {
            "official_or_unknown"
        };
        sqlx::query(
            "INSERT OR REPLACE INTO revision_dependencies(revision_id, dependency_name, dependency_kind, target_package_base, provider_state, candidates_json) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&revision_id)
        .bind(&dependency.name)
        .bind(&dependency.kind)
        .bind(&target)
        .bind(provider_state)
        .bind(if candidates.is_empty() {
            target
                .as_ref()
                .map(|value| json!([value]))
                .unwrap_or_else(|| json!([]))
        } else {
            json!(candidates)
        }
        .to_string())
        .execute(&mut **transaction)
        .await
        .map_err(ApiError::internal)?;
        if let Some(target) = target.filter(|target| target != &snapshot.package_base) {
            sqlx::query("INSERT OR IGNORE INTO subscription_references(owner_package_base, dependency_package_base, created_at) VALUES (?, ?, ?)")
                .bind(&snapshot.package_base)
                .bind(target)
                .bind(now)
                .execute(&mut **transaction)
                .await
                .map_err(ApiError::internal)?;
        }
    }
    Ok(())
}

async fn load_dependency_graph(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<DependencyGraph, ApiError> {
    let mut graph = DependencyGraph::default();
    let packages: Vec<String> = sqlx::query_scalar("SELECT DISTINCT package_base FROM subscriptions WHERE state IN ('active', 'paused', 'retained_without_references')")
        .fetch_all(&mut **transaction).await.map_err(ApiError::internal)?;
    for package in packages {
        graph.add_package(package);
    }
    let rows = sqlx::query(
        "SELECT owner_package_base, dependency_package_base FROM subscription_references",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(ApiError::internal)?;
    for row in rows {
        graph.add_dependency(
            row.get::<String, _>("owner_package_base"),
            row.get::<String, _>("dependency_package_base"),
        );
    }
    Ok(graph)
}

fn subscription_json(row: sqlx::sqlite::SqliteRow) -> Result<Value, ApiError> {
    Ok(json!({
        "id": row.get::<String, _>("id"), "package_base": row.get::<String, _>("package_base"),
        "kind": row.get::<String, _>("kind"), "state": row.get::<String, _>("state"),
        "reference_count": row.get::<i64, _>("reference_count"),
        "followed_outputs": parse_json::<Vec<String>>(row.get("followed_outputs_json"))?,
        "version": row.get::<Option<String>, _>("version"), "description": row.get::<Option<String>, _>("description"),
        "outputs": row.get::<Option<String>, _>("outputs_json").map(|value| parse_json::<Vec<String>>(&value)).transpose()?.unwrap_or_default(),
        "maintainer": row.get::<Option<String>, _>("maintainer"), "out_of_date": row.get::<Option<i64>, _>("out_of_date_at")
    }))
}

fn package_json(package: UpstreamPackage) -> Value {
    json!({
        "name": package.name, "package_base": package.package_base, "version": package.version,
        "description": package.description, "maintainer": package.maintainer, "out_of_date": package.out_of_date,
        "last_modified": package.last_modified, "depends": package.depends, "make_depends": package.make_depends,
        "check_depends": package.check_depends, "opt_depends": package.opt_depends, "provides": package.provides
    })
}

fn vcs_kind(package_base: &str) -> Option<&'static str> {
    ["git", "svn", "hg", "bzr", "cvs", "darcs"]
        .into_iter()
        .find(|kind| package_base.ends_with(&format!("-{kind}")))
}

fn validate_name(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._+-".contains(character))
    {
        return Err(ApiError::bad_request(
            "INVALID_PACKAGE_NAME",
            "软件包名称包含非法字符",
        ));
    }
    Ok(())
}

fn json_string<T: Serialize>(value: &T) -> Result<String, ApiError> {
    serde_json::to_string(value).map_err(ApiError::internal)
}

fn selection_digest(
    snapshot: &UpstreamSnapshot,
    resolutions: &BTreeMap<String, String>,
) -> Result<String, ApiError> {
    let selected: BTreeMap<_, _> = snapshot
        .dependencies
        .iter()
        .filter_map(|dependency| {
            resolutions
                .get(&dependency.name)
                .map(|target| (dependency.name.clone(), target.clone()))
        })
        .collect();
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&selected).map_err(ApiError::internal)?,
    )))
}

fn revision_input_digest(
    snapshot: &UpstreamSnapshot,
    resolutions: &BTreeMap<String, String>,
) -> Result<String, ApiError> {
    let selected: BTreeMap<_, _> = snapshot
        .dependencies
        .iter()
        .filter_map(|dependency| {
            resolutions
                .get(&dependency.name)
                .map(|target| (dependency.name.clone(), target.clone()))
        })
        .collect();
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&json!({
            "snapshot": snapshot,
            "providers": selected,
            "audit_policy_version": "v1"
        }))
        .map_err(ApiError::internal)?,
    )))
}

fn parse_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, ApiError> {
    serde_json::from_str(value).map_err(ApiError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> UpstreamPackage {
        UpstreamPackage {
            name: "demo-cli".into(),
            package_base: "demo".into(),
            version: "1.0-1".into(),
            description: Some("演示".into()),
            maintainer: Some("tester".into()),
            out_of_date: None,
            last_modified: 1,
            depends: vec![],
            make_depends: vec![],
            check_depends: vec![],
            opt_depends: vec![],
            provides: vec![],
        }
    }

    fn snapshot() -> UpstreamSnapshot {
        UpstreamSnapshot {
            package_base: "demo".into(),
            aur_commit: "a".repeat(40),
            vcs_commit: None,
            version: "1.0-1".into(),
            outputs: vec!["demo-cli".into(), "demo-lib".into()],
            dependencies: vec![],
            optional_dependencies: vec![],
            provides: vec![],
            architectures: vec!["x86_64".into()],
            sources: vec![],
            srcinfo: "pkgbase = demo".into(),
        }
    }

    fn empty_closure() -> DependencyClosure {
        DependencyClosure {
            nodes: vec![],
            resolutions: BTreeMap::new(),
            provider_candidates: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn duplicate_subscription_reuses_revision_without_creating_a_batch() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let first = apply_snapshot(
            &database,
            "tester",
            &package(),
            &snapshot(),
            &[],
            &empty_closure(),
        )
        .await
        .unwrap();
        let second = apply_snapshot(
            &database,
            "tester",
            &package(),
            &snapshot(),
            &[],
            &empty_closure(),
        )
        .await
        .unwrap();
        assert_eq!(first["revision_id"], second["revision_id"]);
        assert_eq!(second["idempotent"], true);
        assert!(first["batch_id"].is_string());
        assert!(second["batch_id"].is_null());
        let batch_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM release_batches")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(batch_count, 1);
        let direct: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subscriptions WHERE package_base = 'demo' AND kind = 'direct'",
        )
        .fetch_one(&database)
        .await
        .unwrap();
        assert_eq!(direct, 1);
    }

    #[tokio::test]
    async fn split_output_filter_must_be_a_subset() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let result = apply_snapshot(
            &database,
            "tester",
            &package(),
            &snapshot(),
            &["other".into()],
            &empty_closure(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn aur_dependency_closure_creates_implicit_revision_and_graph_edge() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let mut root = snapshot();
        root.dependencies.push(SnapshotDependency {
            name: "aur-dep".into(),
            kind: "build".into(),
        });
        let dependency_package = UpstreamPackage {
            name: "aur-dep".into(),
            package_base: "aur-dep".into(),
            version: "2.0-1".into(),
            description: None,
            maintainer: Some("tester".into()),
            out_of_date: None,
            last_modified: 2,
            depends: vec![],
            make_depends: vec![],
            check_depends: vec![],
            opt_depends: vec![],
            provides: vec![],
        };
        let dependency_snapshot = UpstreamSnapshot {
            package_base: "aur-dep".into(),
            aur_commit: "b".repeat(40),
            vcs_commit: None,
            version: "2.0-1".into(),
            outputs: vec!["aur-dep".into()],
            dependencies: vec![],
            optional_dependencies: vec![],
            provides: vec![],
            architectures: vec!["x86_64".into()],
            sources: vec![],
            srcinfo: "pkgbase = aur-dep".into(),
        };
        let closure = DependencyClosure {
            nodes: vec![SnapshotNode {
                package: dependency_package,
                snapshot: dependency_snapshot,
            }],
            resolutions: BTreeMap::from([("aur-dep".into(), "aur-dep".into())]),
            provider_candidates: BTreeMap::new(),
        };
        apply_snapshot(&database, "tester", &package(), &root, &[], &closure)
            .await
            .unwrap();
        let implicit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subscriptions WHERE package_base = 'aur-dep' AND kind = 'implicit' AND reference_count = 1",
        )
        .fetch_one(&database)
        .await
        .unwrap();
        let revision_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM revisions WHERE package_base = 'aur-dep'")
                .fetch_one(&database)
                .await
                .unwrap();
        assert_eq!(implicit_count, 1);
        assert_eq!(revision_count, 1);
    }

    #[tokio::test]
    async fn provider_selection_creates_a_new_revision_for_the_same_commit() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let mut root = snapshot();
        root.dependencies.push(SnapshotDependency {
            name: "virtual-api".into(),
            kind: "runtime".into(),
        });
        let blocked = DependencyClosure {
            nodes: vec![],
            resolutions: BTreeMap::new(),
            provider_candidates: BTreeMap::from([(
                "virtual-api".into(),
                vec!["provider-a".into(), "provider-b".into()],
            )]),
        };
        let first = apply_snapshot(&database, "tester", &package(), &root, &[], &blocked)
            .await
            .unwrap();
        assert_eq!(first["batch_state"], "awaiting_provider_selection");

        let selected = DependencyClosure {
            nodes: vec![],
            resolutions: BTreeMap::from([("virtual-api".into(), "provider-a".into())]),
            provider_candidates: BTreeMap::new(),
        };
        let second = apply_snapshot(&database, "tester", &package(), &root, &[], &selected)
            .await
            .unwrap();
        assert_ne!(first["revision_id"], second["revision_id"]);
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state FROM revisions WHERE package_base = 'demo' ORDER BY created_at",
        )
        .fetch_all(&database)
        .await
        .unwrap();
        assert!(states.contains(&"superseded".to_owned()));
        assert!(states.contains(&"discovered".to_owned()));
    }
}
