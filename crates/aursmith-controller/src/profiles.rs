use crate::{auth, error::ApiError, routes::AppState};
use aursmith_domain::{DependencyAction, DependencyStats, ProfilePolicy};
use aursmith_protocol::{BuildProfileSpec, SignedEnvelope};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AuthorizeRequest {
    name: String,
    spec: BuildProfileSpec,
}

pub async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<AuthorizeRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    if request.name.trim().is_empty() || request.name.len() > 64 {
        return Err(ApiError::bad_request(
            "INVALID_PROFILE_NAME",
            "Profile 名称长度必须为 1 至 64",
        ));
    }
    validate_spec(&request.spec)?;
    request.spec.profile_sha256 = request.spec.content_sha256().map_err(ApiError::internal)?;
    let envelope =
        SignedEnvelope::sign("aursmith.build_profile", &request.spec, &state.signing_key)
            .map_err(ApiError::internal)?;
    let envelope_json = serde_json::to_string(&envelope).map_err(ApiError::internal)?;
    let id = Uuid::new_v4().to_string();
    let job_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO build_profiles(id, name, architecture, runner, manifest_sha256, state, package_manifest_json, envelope_json, created_at) VALUES (?, ?, 'x86_64', 'kvm', ?, 'candidate', ?, ?, ?)")
        .bind(&id).bind(request.name.trim()).bind(&request.spec.profile_sha256)
        .bind(serde_json::to_string(&request.spec.installed_packages).map_err(ApiError::internal)?)
        .bind(&envelope_json).bind(now).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO jobs(id, required_role, status, priority, revision_sha256, kind, profile_sha256, source_manifest_sha256, dependency_snapshot_sha256, inputs_json, required_labels_json, limits_json, created_at, updated_at) VALUES (?, 'builder', 'queued', 100, ?, 'profile_fixture', ?, ?, ?, '[]', '[]', ?, ?, ?)")
        .bind(&job_id).bind(&request.spec.profile_sha256).bind(&request.spec.profile_sha256)
        .bind("0".repeat(64)).bind("0".repeat(64))
        .bind(r#"{"cpu_count":1,"memory_mib":1024,"disk_mib":4096,"timeout_seconds":300}"#)
        .bind(now).bind(now).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    crate::routes::append_event_in_transaction(
        &mut transaction,
        "build_profile",
        &id,
        "profile_authorized",
        json!({"profile_sha256": request.spec.profile_sha256}),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": id, "profile_sha256": request.spec.profile_sha256, "fixture_job_id": job_id, "envelope": envelope}),
        ),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT id, name, architecture, manifest_sha256, state, package_manifest_json, created_at, activated_at, last_verified_at, failure_reason FROM build_profiles ORDER BY created_at DESC")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "id": row.get::<String,_>("id"), "name": row.get::<String,_>("name"), "architecture": row.get::<String,_>("architecture"),
        "profile_sha256": row.get::<String,_>("manifest_sha256"), "state": row.get::<String,_>("state"),
        "packages": serde_json::from_str::<Value>(row.get("package_manifest_json")).unwrap_or(Value::Null),
        "created_at": row.get::<String,_>("created_at"), "activated_at": row.get::<Option<String>,_>("activated_at"),
        "last_verified_at": row.get::<Option<String>,_>("last_verified_at"), "failure_reason": row.get::<Option<String>,_>("failure_reason")
    })).collect::<Vec<_>>() })))
}

pub async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let row = sqlx::query(
        "SELECT state, manifest_sha256, last_verified_at FROM build_profiles WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("Profile 不存在"))?;
    if row.get::<String, _>("state") == "failed" {
        return Err(ApiError::conflict(
            "PROFILE_FAILED",
            "验证失败的 Profile 不能激活",
        ));
    }
    if row.get::<Option<String>, _>("last_verified_at").is_none() {
        return Err(ApiError::conflict(
            "PROFILE_NOT_VERIFIED",
            "Profile 必须先通过启动、网络隔离和 fixture build",
        ));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM build_profiles WHERE state = 'active' AND id != ?",
    )
    .bind(&id)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if active >= 4 {
        return Err(ApiError::conflict(
            "PROFILE_LIMIT",
            "最多允许四个活跃 Profile，请先停用一个",
        ));
    }
    sqlx::query("UPDATE build_profiles SET state = 'active', activated_at = ? WHERE id = ?")
        .bind(Utc::now())
        .bind(&id)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    crate::routes::append_event(
        &state.database,
        "build_profile",
        &id,
        "profile_activated",
        json!({"profile_sha256": row.get::<String,_>("manifest_sha256")}),
        &actor,
    )
    .await?;
    Ok(Json(json!({"id": id, "state": "active"})))
}

pub async fn deactivate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let row = sqlx::query("SELECT state, manifest_sha256 FROM build_profiles WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Profile 不存在"))?;
    if row.get::<String, _>("state") != "active" {
        return Err(ApiError::conflict(
            "PROFILE_NOT_ACTIVE",
            "只有 active Profile 可以停用",
        ));
    }
    let active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM build_profiles WHERE state = 'active'")
            .fetch_one(&state.database)
            .await
            .map_err(ApiError::internal)?;
    if active <= 1 {
        return Err(ApiError::conflict(
            "LAST_ACTIVE_PROFILE",
            "至少保留一个 active Profile",
        ));
    }
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE build_profiles SET state = 'inactive' WHERE id = ? AND state = 'active'")
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    crate::routes::append_event_in_transaction(
        &mut transaction,
        "build_profile",
        &id,
        "profile_deactivated",
        json!({"profile_sha256": row.get::<String,_>("manifest_sha256")}),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(Json(json!({"id": id, "state": "inactive"})))
}

pub async fn recommendations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    evaluate_dependencies(&state.database).await?;
    let rows = sqlx::query("SELECT package_name, action, stats_json, consecutive_hot_periods, consecutive_low_periods, evaluated_at FROM profile_dependency_evaluations ORDER BY action, package_name")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({
        "policy": ProfilePolicy::default(),
        "items": rows.into_iter().map(|row| json!({
            "package_name": row.get::<String,_>("package_name"),
            "action": row.get::<String,_>("action"),
            "stats": serde_json::from_str::<Value>(row.get("stats_json")).unwrap_or(Value::Null),
            "consecutive_hot_periods": row.get::<i64,_>("consecutive_hot_periods"),
            "consecutive_low_periods": row.get::<i64,_>("consecutive_low_periods"),
            "evaluated_at": row.get::<String,_>("evaluated_at"),
        })).collect::<Vec<_>>()
    })))
}

pub(crate) async fn evaluate_dependencies(database: &sqlx::SqlitePool) -> Result<(), ApiError> {
    let now = Utc::now();
    let last: Option<String> =
        sqlx::query_scalar("SELECT MAX(evaluated_at) FROM profile_evaluation_runs")
            .fetch_one(database)
            .await
            .map_err(ApiError::internal)?;
    if last
        .and_then(|value| value.parse::<chrono::DateTime<Utc>>().ok())
        .is_some_and(|value| value > now - Duration::days(7))
    {
        return Ok(());
    }
    let successful_builds: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE kind = 'build' AND status = 'succeeded'",
    )
    .fetch_one(database)
    .await
    .map_err(ApiError::internal)?;
    let active_profiles = sqlx::query_scalar::<_, String>(
        "SELECT package_manifest_json FROM build_profiles WHERE state = 'active'",
    )
    .fetch_all(database)
    .await
    .map_err(ApiError::internal)?;
    let mut baked = BTreeSet::new();
    for manifest in active_profiles {
        if let Ok(packages) = serde_json::from_str::<Vec<String>>(&manifest) {
            baked.extend(packages);
        }
    }
    let observations = sqlx::query("SELECT package_name, COUNT(*) AS uses_total, SUM(CASE WHEN observed_at >= ? THEN 1 ELSE 0 END) AS uses_month, SUM(CASE WHEN job_id IN (SELECT job_id FROM dependency_observations GROUP BY job_id ORDER BY MAX(observed_at) DESC LIMIT 20) THEN 1 ELSE 0 END) AS uses_recent, COALESCE(SUM(download_bytes), 0) AS download_bytes, COALESCE(SUM(download_milliseconds + install_milliseconds), 0) AS elapsed_milliseconds, COALESCE(SUM(cache_hit), 0) AS cache_hits, MAX(observed_at) AS last_used_at FROM dependency_observations WHERE official_repository = 1 GROUP BY package_name ORDER BY package_name")
        .bind(now - Duration::days(30)).fetch_all(database).await.map_err(ApiError::internal)?;
    let policy = ProfilePolicy::default();
    let mut transaction = database.begin().await.map_err(ApiError::internal)?;
    for row in observations {
        let package_name: String = row.get("package_name");
        let uses_total = u32::try_from(row.get::<i64, _>("uses_total")).unwrap_or(u32::MAX);
        let elapsed = u64::try_from(row.get::<i64, _>("elapsed_milliseconds")).unwrap_or_default();
        let previous = sqlx::query("SELECT consecutive_hot_periods, consecutive_low_periods FROM profile_dependency_evaluations WHERE package_name = ?")
            .bind(&package_name).fetch_optional(&mut *transaction).await.map_err(ApiError::internal)?;
        let last_used_at = row
            .get::<String, _>("last_used_at")
            .parse::<chrono::DateTime<Utc>>()
            .unwrap_or(now);
        let mut stats = DependencyStats {
            total_observations: u32::try_from(successful_builds).unwrap_or(u32::MAX),
            uses_in_recent_window: u32::try_from(row.get::<i64, _>("uses_recent"))
                .unwrap_or(u32::MAX),
            uses_this_month: u32::try_from(row.get::<i64, _>("uses_month")).unwrap_or(u32::MAX),
            estimated_saved_seconds: elapsed
                .checked_div(u64::from(uses_total))
                .unwrap_or_default()
                / 1000,
            consecutive_add_periods: 0,
            consecutive_low_periods: 0,
            days_since_last_use: u32::try_from((now - last_used_at).num_days().max(0))
                .unwrap_or(u32::MAX),
            currently_baked: baked.contains(&package_name),
            official_repository_package: true,
        };
        let provisional = policy.evaluate(stats);
        let hot = matches!(
            provisional,
            DependencyAction::SuggestAdd | DependencyAction::Add | DependencyAction::Keep
        );
        let previous_hot = previous
            .as_ref()
            .map(|row| row.get::<i64, _>("consecutive_hot_periods"))
            .unwrap_or_default();
        let previous_low = previous
            .as_ref()
            .map(|row| row.get::<i64, _>("consecutive_low_periods"))
            .unwrap_or_default();
        stats.consecutive_add_periods = if hot {
            u8::try_from(previous_hot + 1).unwrap_or(u8::MAX)
        } else {
            0
        };
        stats.consecutive_low_periods = if hot {
            0
        } else {
            u8::try_from(previous_low + 1).unwrap_or(u8::MAX)
        };
        let action = policy.evaluate(stats);
        let action_name = serde_json::to_value(action)
            .map_err(ApiError::internal)?
            .as_str()
            .unwrap_or("observe_only")
            .to_owned();
        let stats_json = json!({
            "successful_builds": successful_builds,
            "uses_total": uses_total,
            "uses_recent": stats.uses_in_recent_window,
            "uses_this_month": stats.uses_this_month,
            "download_bytes": row.get::<i64,_>("download_bytes"),
            "average_saved_seconds": stats.estimated_saved_seconds,
            "cache_hits": row.get::<i64,_>("cache_hits"),
            "days_since_last_use": stats.days_since_last_use,
            "currently_baked": stats.currently_baked,
        })
        .to_string();
        sqlx::query("INSERT INTO profile_dependency_evaluations(package_name, consecutive_hot_periods, consecutive_low_periods, action, stats_json, evaluated_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(package_name) DO UPDATE SET consecutive_hot_periods = excluded.consecutive_hot_periods, consecutive_low_periods = excluded.consecutive_low_periods, action = excluded.action, stats_json = excluded.stats_json, evaluated_at = excluded.evaluated_at")
            .bind(package_name).bind(i64::from(stats.consecutive_add_periods)).bind(i64::from(stats.consecutive_low_periods))
            .bind(action_name).bind(stats_json).bind(now).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    sqlx::query("INSERT INTO profile_evaluation_runs(id, evaluated_at) VALUES (?, ?)")
        .bind(Uuid::new_v4().to_string())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(())
}

fn validate_spec(spec: &BuildProfileSpec) -> Result<(), ApiError> {
    for (entry, expected) in [
        (&spec.root_image, "root.qcow2"),
        (&spec.kernel, "vmlinuz-linux"),
        (&spec.initramfs, "initramfs-linux.img"),
    ] {
        if entry.path != expected
            || entry.sha256.len() != 64
            || !entry.sha256.chars().all(|value| value.is_ascii_hexdigit())
            || entry.size == 0
        {
            return Err(ApiError::bad_request(
                "INVALID_PROFILE_MANIFEST",
                format!("Profile 文件声明无效：{expected}"),
            ));
        }
    }
    if spec.installed_packages.is_empty() || spec.installed_packages.len() > 4096 {
        return Err(ApiError::bad_request(
            "INVALID_PROFILE_PACKAGES",
            "Profile 包清单必须包含 1 至 4096 项",
        ));
    }
    if let Some(repository_mirror) = &spec.repository_mirror {
        let mirror = url::Url::parse(repository_mirror).map_err(|_| {
            ApiError::bad_request("INVALID_PROFILE_MIRROR", "Profile 镜像地址不是有效 URL")
        })?;
        if mirror.scheme() != "https"
            || mirror.host_str().is_none()
            || !mirror.username().is_empty()
            || mirror.password().is_some()
            || mirror.query().is_some()
            || mirror.fragment().is_some()
        {
            return Err(ApiError::bad_request(
                "INVALID_PROFILE_MIRROR",
                "Profile 镜像地址必须是无凭据、查询参数和片段的 HTTPS Base URL",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hot_dependency_requires_two_weekly_evaluations() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        for index in 0..20 {
            let job_id = Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO jobs(id, required_role, status, priority, kind, inputs_json, required_labels_json, limits_json, created_at, updated_at) VALUES (?, 'builder', 'succeeded', 0, 'build', '[]', '[]', '{}', ?, ?)")
                .bind(&job_id).bind(now).bind(now).execute(&database).await.unwrap();
            if index < 6 {
                sqlx::query("INSERT INTO dependency_observations(id, job_id, package_name, official_repository, download_bytes, download_milliseconds, install_milliseconds, cache_hit, observed_at) VALUES (?, ?, 'cmake', 1, 1000, 60000, 0, 0, ?)")
                    .bind(Uuid::new_v4().to_string()).bind(job_id).bind(now).execute(&database).await.unwrap();
            }
        }
        evaluate_dependencies(&database).await.unwrap();
        let first: String = sqlx::query_scalar(
            "SELECT action FROM profile_dependency_evaluations WHERE package_name = 'cmake'",
        )
        .fetch_one(&database)
        .await
        .unwrap();
        assert_eq!(first, "suggest_add");
        sqlx::query("UPDATE profile_evaluation_runs SET evaluated_at = ?")
            .bind(now - Duration::days(8))
            .execute(&database)
            .await
            .unwrap();
        evaluate_dependencies(&database).await.unwrap();
        let second: String = sqlx::query_scalar(
            "SELECT action FROM profile_dependency_evaluations WHERE package_name = 'cmake'",
        )
        .fetch_one(&database)
        .await
        .unwrap();
        assert_eq!(second, "add");
    }

    #[test]
    fn profile_mirror_requires_https_without_embedded_credentials() {
        let entry = |path: &str| aursmith_protocol::ManifestEntry {
            path: path.into(),
            sha256: "a".repeat(64),
            size: 1,
        };
        let mut spec = BuildProfileSpec {
            profile_sha256: String::new(),
            root_image: entry("root.qcow2"),
            kernel: entry("vmlinuz-linux"),
            initramfs: entry("initramfs-linux.img"),
            installed_packages: vec!["base 3-3".into()],
            repository_mirror: Some("http://mirror.example.org".into()),
            created_at: Utc::now(),
        };
        assert!(validate_spec(&spec).is_err());
        spec.repository_mirror = Some("https://mirror.example.org/archlinux".into());
        assert!(validate_spec(&spec).is_ok());
    }
}
