use crate::{auth, error::ApiError, routes::AppState};
use aursmith_protocol::{BuildProfileSpec, SignedEnvelope};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
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
    sqlx::query("INSERT INTO build_profiles(id, name, architecture, runner, manifest_sha256, state, package_manifest_json, envelope_json, created_at) VALUES (?, ?, 'x86_64', 'kvm', ?, 'candidate', ?, ?, ?)")
        .bind(&id).bind(request.name.trim()).bind(&request.spec.profile_sha256)
        .bind(serde_json::to_string(&request.spec.installed_packages).map_err(ApiError::internal)?)
        .bind(&envelope_json).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    crate::routes::append_event(
        &state.database,
        "build_profile",
        &id,
        "profile_authorized",
        json!({"profile_sha256": request.spec.profile_sha256}),
        &actor,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": id, "profile_sha256": request.spec.profile_sha256, "envelope": envelope}),
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
    Ok(())
}
