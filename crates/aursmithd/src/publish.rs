//! 期望状态发布与后台调和循环。
//!
//! 每轮调和：过期租约 → 排队构建 → 计算期望软件包集合 → 与最近一次发布比较 →
//! 不同且没有影响它的进行中构建时，把 PublishPlan 写入签名器 inbox → 读取签名器 outbox。
//! 没有批次、没有发布任务表，重试就是下一轮调和。

use crate::{
    app::AppState,
    builds::{artifact_dir, expire_leases, has_active_build, schedule, usable_build},
    error::ApiError,
    packages::{Closure, compute_closure},
};
use anyhow::Context;
use aursmith_core::protocol::{
    ArtifactRecord, PUBLISH_PLAN_FORMAT, PublishPlan, SignerResult, SignerState,
};
use axum::{Json, extract::State};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// 同一失败计划的自动重试间隔。
const FAILED_PLAN_BACKOFF_MINUTES: i64 = 15;
/// 签名器多久没有结果视为失败。
const SIGNER_TIMEOUT_MINUTES: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    pub plan: PublishPlan,
    /// artifact 文件名 -> 来源 build id。
    pub sources: BTreeMap<String, String>,
    /// 因依赖不可用而暂不发布的 pkgbase。
    pub withheld: BTreeSet<String>,
}

/// 计算期望仓库状态：闭包内每个 pkgbase 选用当前可用的构建；
/// 自身或任一 AUR 依赖没有可用构建的 pkgbase 暂不发布。
pub async fn desired_state(state: &AppState, closure: &Closure) -> anyhow::Result<Desired> {
    let mut chosen: BTreeMap<String, (String, Vec<ArtifactRecord>)> = BTreeMap::new();
    for base in &closure.members {
        if let Some(build) = usable_build(state, base).await? {
            chosen.insert(base.clone(), build);
        }
    }
    let mut withheld = BTreeSet::new();
    for base in &closure.members {
        let missing = closure
            .graph
            .closure([base.as_str()])
            .iter()
            .any(|member| !chosen.contains_key(member));
        if missing {
            withheld.insert(base.clone());
        }
    }
    let mut artifacts = Vec::new();
    let mut sources = BTreeMap::new();
    let mut names = BTreeSet::new();
    for (base, (build_id, records)) in &chosen {
        if withheld.contains(base) {
            continue;
        }
        for artifact in records {
            if !names.insert(artifact.package_name.clone()) {
                anyhow::bail!("多个 pkgbase 产出同名软件包 {}", artifact.package_name);
            }
            sources.insert(artifact.file.clone(), build_id.clone());
            artifacts.push(artifact.clone());
        }
    }
    artifacts.sort();
    Ok(Desired {
        plan: PublishPlan {
            format: PUBLISH_PLAN_FORMAT,
            repository_name: state.config.repository_name.clone(),
            artifacts,
        },
        sources,
        withheld,
    })
}

/// 一轮调和。返回本轮是否写出了新的发布计划。
pub async fn reconcile(state: &AppState) -> anyhow::Result<bool> {
    collect_signer_results(state).await?;
    expire_leases(state).await?;
    let closure = compute_closure(&state.db).await?;
    schedule(state, &closure).await?;
    for base in &closure.members {
        if has_active_build(state, &closure, base).await? {
            return Ok(false);
        }
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM publications WHERE state = 'pending'")
            .fetch_one(&state.db)
            .await?;
    if pending > 0 {
        return Ok(false);
    }
    let desired = desired_state(state, &closure).await?;
    let plan_sha256 = desired.plan.sha256();
    let latest = sqlx::query(
        "SELECT plan_sha256, state, finished_at FROM publications ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?;
    match &latest {
        None if desired.plan.artifacts.is_empty() => return Ok(false),
        Some(row) if row.get::<String, _>("plan_sha256") == plan_sha256 => {
            let failed_recently = row.get::<String, _>("state") == "failed"
                && row
                    .get::<Option<chrono::DateTime<Utc>>, _>("finished_at")
                    .is_some_and(|finished| {
                        finished > Utc::now() - Duration::minutes(FAILED_PLAN_BACKOFF_MINUTES)
                    });
            if row.get::<String, _>("state") == "published" || failed_recently {
                return Ok(false);
            }
        }
        _ => {}
    }
    write_plan(state, &desired, &plan_sha256).await?;
    Ok(true)
}

/// 把计划与签名器尚未持有的产物写入 `inbox/<plan_sha256>/`（先写临时目录再原子改名）。
async fn write_plan(state: &AppState, desired: &Desired, plan_sha256: &str) -> anyhow::Result<()> {
    let published: BTreeSet<String> = match sqlx::query_scalar::<_, String>(
        "SELECT plan_json FROM publications WHERE state = 'published' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?
    {
        Some(json) => serde_json::from_str::<PublishPlan>(&json)?
            .artifacts
            .into_iter()
            .map(|artifact| artifact.sha256)
            .collect(),
        None => BTreeSet::new(),
    };
    let inbox = state.config.exchange_dir.join("inbox");
    let temporary = inbox.join(format!(".tmp-{plan_sha256}"));
    let final_path = inbox.join(plan_sha256);
    if temporary.exists() {
        tokio::fs::remove_dir_all(&temporary).await?;
    }
    if final_path.exists() {
        tokio::fs::remove_dir_all(&final_path).await?;
    }
    tokio::fs::create_dir_all(&temporary).await?;
    for artifact in &desired.plan.artifacts {
        if published.contains(&artifact.sha256) {
            continue;
        }
        let build_id = &desired.sources[&artifact.file];
        tokio::fs::copy(
            artifact_dir(state, build_id).join(&artifact.file),
            temporary.join(&artifact.file),
        )
        .await
        .with_context(|| format!("无法复制产物 {}", artifact.file))?;
    }
    tokio::fs::write(temporary.join("plan.json"), desired.plan.canonical_bytes()).await?;
    tokio::fs::rename(&temporary, &final_path).await?;
    sqlx::query("INSERT INTO publications(id, plan_sha256, plan_json, state, created_at) VALUES (?, ?, ?, 'pending', ?)")
        .bind(Uuid::new_v4().to_string())
        .bind(plan_sha256)
        .bind(String::from_utf8(desired.plan.canonical_bytes())?)
        .bind(Utc::now())
        .execute(&state.db)
        .await?;
    tracing::info!(plan = %plan_sha256, artifacts = desired.plan.artifacts.len(), "已写出发布计划");
    Ok(())
}

/// 读取签名器结果并清理 inbox/outbox。
pub async fn collect_signer_results(state: &AppState) -> anyhow::Result<()> {
    let pending =
        sqlx::query("SELECT id, plan_sha256, created_at FROM publications WHERE state = 'pending'")
            .fetch_all(&state.db)
            .await?;
    for row in pending {
        let id: String = row.get("id");
        let plan_sha256: String = row.get("plan_sha256");
        let outbox = state
            .config
            .exchange_dir
            .join("outbox")
            .join(format!("{plan_sha256}.json"));
        let result = match tokio::fs::read(&outbox).await {
            Ok(bytes) => match serde_json::from_slice::<SignerResult>(&bytes) {
                Ok(result) if result.plan_sha256 == plan_sha256 => Some(result),
                _ => Some(SignerResult {
                    plan_sha256: plan_sha256.clone(),
                    state: SignerState::Failed,
                    error: Some("签名器结果格式无效".into()),
                    manifest_sha256: None,
                    keyring_fingerprint: None,
                    finished_at: Utc::now(),
                }),
            },
            Err(_)
                if row.get::<chrono::DateTime<Utc>, _>("created_at")
                    < Utc::now() - Duration::minutes(SIGNER_TIMEOUT_MINUTES) =>
            {
                Some(SignerResult {
                    plan_sha256: plan_sha256.clone(),
                    state: SignerState::Failed,
                    error: Some("签名器超时未返回结果".into()),
                    manifest_sha256: None,
                    keyring_fingerprint: None,
                    finished_at: Utc::now(),
                })
            }
            Err(_) => None,
        };
        let Some(result) = result else { continue };
        let next = match result.state {
            SignerState::Published => "published",
            SignerState::Failed => "failed",
        };
        sqlx::query("UPDATE publications SET state = ?, error = ?, manifest_sha256 = ?, keyring_fingerprint = ?, finished_at = ? WHERE id = ?")
            .bind(next)
            .bind(&result.error)
            .bind(&result.manifest_sha256)
            .bind(&result.keyring_fingerprint)
            .bind(result.finished_at)
            .bind(&id)
            .execute(&state.db)
            .await?;
        tracing::info!(plan = %plan_sha256, state = next, error = ?result.error, "签名器返回结果");
        tokio::fs::remove_file(&outbox).await.ok();
        tokio::fs::remove_dir_all(state.config.exchange_dir.join("inbox").join(&plan_sha256))
            .await
            .ok();
    }
    Ok(())
}

/// 删除不再可能被引用的构建产物：保留每个 pkgbase 的可用构建与最近三次发布引用的构建。
pub async fn collect_garbage(state: &AppState) -> anyhow::Result<usize> {
    let closure = compute_closure(&state.db).await?;
    let mut keep = BTreeSet::new();
    for base in &closure.members {
        if let Some((id, _)) = usable_build(state, base).await? {
            keep.insert(id);
        }
    }
    let recent: Vec<String> =
        sqlx::query_scalar("SELECT plan_json FROM publications ORDER BY created_at DESC LIMIT 3")
            .fetch_all(&state.db)
            .await?;
    let referenced: BTreeSet<String> = recent
        .iter()
        .filter_map(|json| serde_json::from_str::<PublishPlan>(json).ok())
        .flat_map(|plan| plan.artifacts.into_iter().map(|artifact| artifact.sha256))
        .collect();
    let succeeded = sqlx::query("SELECT id, artifacts_json FROM builds WHERE state = 'succeeded'")
        .fetch_all(&state.db)
        .await?;
    for row in succeeded {
        let artifacts: Vec<ArtifactRecord> =
            serde_json::from_str(row.get("artifacts_json")).unwrap_or_default();
        if artifacts
            .iter()
            .any(|artifact| referenced.contains(&artifact.sha256))
        {
            keep.insert(row.get("id"));
        }
    }
    let root = state.config.data_dir.join("artifacts");
    let mut removed = 0;
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return Ok(0);
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let active: Option<String> = sqlx::query_scalar("SELECT state FROM builds WHERE id = ?")
            .bind(&name)
            .fetch_optional(&state.db)
            .await?;
        let in_progress = matches!(active.as_deref(), Some("running" | "uploading"));
        if !keep.contains(&name) && !in_progress {
            tokio::fs::remove_dir_all(entry.path()).await.ok();
            removed += 1;
        }
    }
    Ok(removed)
}

/// 后台调和循环：每 15 秒或被唤醒时运行；每小时清理一次产物。
pub async fn run_loop(state: AppState) {
    let mut last_gc = Utc::now();
    loop {
        if let Err(error) = reconcile(&state).await {
            tracing::error!(error = %format!("{error:#}"), "调和循环失败");
        }
        if Utc::now() - last_gc > Duration::hours(1) {
            match collect_garbage(&state).await {
                Ok(removed) if removed > 0 => tracing::info!(removed, "已清理过期构建产物"),
                Ok(_) => {}
                Err(error) => tracing::error!(error = %error, "产物清理失败"),
            }
            last_gc = Utc::now();
        }
        tokio::select! {
            () = state.wake.notified() => {}
            () = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
        }
    }
}

// ---------------------------------------------------------------- 管理 API

pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query("SELECT * FROM publications ORDER BY created_at DESC LIMIT 50")
        .fetch_all(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let current = rows
        .iter()
        .position(|row| row.get::<String, _>("state") == "published");
    let items: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let plan: Option<PublishPlan> = serde_json::from_str(row.get("plan_json")).ok();
            json!({
                "id": row.get::<String, _>("id"),
                "plan_sha256": row.get::<String, _>("plan_sha256"),
                "state": row.get::<String, _>("state"),
                "current": Some(index) == current,
                "error": row.get::<Option<String>, _>("error"),
                "manifest_sha256": row.get::<Option<String>, _>("manifest_sha256"),
                "keyring_fingerprint": row.get::<Option<String>, _>("keyring_fingerprint"),
                "artifacts": plan.map(|plan| plan.artifacts).unwrap_or_default(),
                "created_at": row.get::<String, _>("created_at"),
                "finished_at": row.get::<Option<String>, _>("finished_at"),
            })
        })
        .collect();
    let closure = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let desired = desired_state(&state, &closure)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "items": items,
        "desired": {
            "plan_sha256": desired.plan.sha256(),
            "artifact_count": desired.plan.artifacts.len(),
            "withheld": desired.withheld,
        },
    })))
}
