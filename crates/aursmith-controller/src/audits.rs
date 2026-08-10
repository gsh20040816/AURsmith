use crate::{auth, error::ApiError, routes::AppState};
use aursmith_domain::{AgentVerdict, AuditDecision, LowCostRoute};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ManualDecisionRequest {
    approve: bool,
    rationale: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RunnerResponse {
    verdict: String,
    summary: String,
    findings: Vec<Value>,
    files_read: Vec<String>,
    adapter: String,
    provider: String,
    model: String,
    adapter_version: String,
    raw_output: Value,
    raw_output_sha256: String,
    #[serde(default)]
    cost_microusd: Option<i64>,
}

pub async fn dispatch_one(state: &AppState) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT agent_runs.id, agent_runs.audit_bundle_sha256, agent_runs.tier, agent_runs.slot, agent_runs.attempt, audit_bundles.payload_json, audit_bundles.coverage_json, audit_bundles.deterministic_findings_json FROM agent_runs JOIN audit_bundles ON audit_bundles.sha256 = agent_runs.audit_bundle_sha256 WHERE agent_runs.status = 'pending' ORDER BY CASE agent_runs.tier WHEN 'low' THEN 0 ELSE 1 END, agent_runs.started_at, agent_runs.slot LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(row) = row else {
        return Ok(());
    };
    let run_id: String = row.get("id");
    let bundle_sha256: String = row.get("audit_bundle_sha256");
    if !budget_available(state).await? {
        sqlx::query("UPDATE agent_runs SET status = 'failed', verdict = 'error', raw_output_json = ?, finished_at = ? WHERE audit_bundle_sha256 = ? AND status = 'pending'")
            .bind(json!({"error": "AGENT_BUDGET_EXCEEDED"}).to_string()).bind(Utc::now()).bind(&bundle_sha256)
            .execute(&state.database).await.map_err(ApiError::internal)?;
        finalize(state, &bundle_sha256, AuditDecision::ManualReview).await?;
        return Ok(());
    }
    let claimed = sqlx::query("UPDATE agent_runs SET status = 'running', started_at = ? WHERE id = ? AND status = 'pending'")
        .bind(Utc::now()).bind(&run_id).execute(&state.database).await.map_err(ApiError::internal)?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let tier: String = row.get("tier");
    let slot: i64 = row.get("slot");
    let endpoint = if tier == "low" {
        state
            .config
            .low_agent_endpoints
            .get(usize::try_from(slot - 1).unwrap_or(usize::MAX))
            .cloned()
    } else if state.config.high_agent_endpoint.is_empty() {
        None
    } else {
        Some(state.config.high_agent_endpoint.clone())
    };
    let request = json!({
        "bundle_sha256": bundle_sha256,
        "payload": parse_json::<Value>(row.get("payload_json"))?,
        "coverage": parse_json::<Value>(row.get("coverage_json"))?,
        "deterministic_findings": parse_json::<Value>(row.get("deterministic_findings_json"))?,
        "normalized_objections": if tier == "high" { low_cost_objections(state, &bundle_sha256).await? } else { vec![] }
    });
    let result = match endpoint {
        Some(endpoint) => invoke_runner(&endpoint, &request).await,
        None => Err("对应 Agent Runner 未配置".to_owned()),
    };
    match result {
        Ok(response) => record_success(state, &run_id, &bundle_sha256, &tier, response).await?,
        Err(error) => {
            record_failure(
                state,
                &run_id,
                &bundle_sha256,
                &tier,
                slot,
                row.get("attempt"),
                &error,
            )
            .await?
        }
    }
    Ok(())
}

async fn budget_available(state: &AppState) -> Result<bool, ApiError> {
    let daily: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs WHERE started_at >= datetime('now', 'start of day')",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let monthly: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs WHERE started_at >= datetime('now', 'start of month')",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let monthly_cost: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_microusd), 0) FROM agent_runs WHERE started_at >= datetime('now', 'start of month')")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    let daily_limit = crate::routes::effective_i64_setting(
        state,
        "agent_daily_call_limit",
        state.config.agent_daily_call_limit,
    )
    .await?;
    let monthly_limit = crate::routes::effective_i64_setting(
        state,
        "agent_monthly_call_limit",
        state.config.agent_monthly_call_limit,
    )
    .await?;
    let monthly_cost_limit = crate::routes::effective_i64_setting(
        state,
        "agent_monthly_cost_limit_microusd",
        state.config.agent_monthly_cost_limit_microusd,
    )
    .await?;
    Ok(daily < daily_limit && monthly < monthly_limit && monthly_cost < monthly_cost_limit)
}

async fn invoke_runner(endpoint: &str, request: &Value) -> Result<RunnerResponse, String> {
    let url = format!("{}/v1/audit", endpoint.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(190))
        .build()
        .map_err(|error| error.to_string())?
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Runner HTTP {}：{}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response.json().await.map_err(|error| error.to_string())
}

async fn record_success(
    state: &AppState,
    run_id: &str,
    bundle: &str,
    tier: &str,
    response: RunnerResponse,
) -> Result<(), ApiError> {
    let verdict = match response.verdict.as_str() {
        "approve" => "approve",
        "reject" => "reject",
        _ => "error",
    };
    let report = serde_json::to_value(&response).map_err(ApiError::internal)?;
    let report_sha = hex::encode(Sha256::digest(
        serde_json::to_vec(&report).map_err(ApiError::internal)?,
    ));
    sqlx::query("UPDATE agent_runs SET status = 'succeeded', verdict = ?, adapter = ?, provider = ?, model = ?, adapter_version = ?, report_json = ?, raw_output_json = ?, report_sha256 = ?, cost_microusd = ?, finished_at = ? WHERE id = ?")
        .bind(verdict).bind(&response.adapter).bind(&response.provider).bind(&response.model).bind(&response.adapter_version)
        .bind(report.to_string()).bind(response.raw_output.to_string()).bind(report_sha).bind(response.cost_microusd).bind(Utc::now()).bind(run_id)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    evaluate(state, bundle, tier).await
}

async fn record_failure(
    state: &AppState,
    run_id: &str,
    bundle: &str,
    tier: &str,
    slot: i64,
    attempt: i64,
    error: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE agent_runs SET status = 'failed', verdict = 'error', raw_output_json = ?, finished_at = ? WHERE id = ?")
        .bind(json!({"error": error}).to_string()).bind(Utc::now()).bind(run_id)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    if attempt == 0 {
        sqlx::query("INSERT INTO agent_runs(id, audit_bundle_sha256, tier, slot, attempt, adapter, model, adapter_version, prompt_version, status) VALUES (?, ?, ?, ?, 1, 'unconfigured', 'unconfigured', 'v1', 'v1', 'pending')")
            .bind(Uuid::new_v4().to_string()).bind(bundle).bind(tier).bind(slot)
            .execute(&state.database).await.map_err(ApiError::internal)?;
        return Ok(());
    }
    evaluate(state, bundle, tier).await
}

async fn evaluate(state: &AppState, bundle: &str, tier: &str) -> Result<(), ApiError> {
    if tier == "high" {
        let verdict: Option<String> = sqlx::query_scalar("SELECT verdict FROM agent_runs WHERE audit_bundle_sha256 = ? AND tier = 'high' AND status IN ('succeeded', 'failed') ORDER BY attempt DESC LIMIT 1")
            .bind(bundle).fetch_optional(&state.database).await.map_err(ApiError::internal)?.flatten();
        if let Some(verdict) = verdict {
            let decision = if verdict == "approve" {
                AuditDecision::ApprovedByHighCost
            } else {
                AuditDecision::ManualReview
            };
            finalize(state, bundle, decision).await?;
        }
        return Ok(());
    }
    let rows = sqlx::query("SELECT slot, verdict FROM agent_runs AS run WHERE audit_bundle_sha256 = ? AND tier = 'low' AND status IN ('succeeded', 'failed') AND attempt = (SELECT MAX(attempt) FROM agent_runs WHERE audit_bundle_sha256 = run.audit_bundle_sha256 AND tier = 'low' AND slot = run.slot) ORDER BY slot")
        .bind(bundle).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    if rows.len() != 3 {
        return Ok(());
    }
    let mut verdicts = [AgentVerdict::Error; 3];
    for row in rows {
        let slot: i64 = row.get("slot");
        verdicts[usize::try_from(slot - 1).map_err(ApiError::internal)?] =
            match row.get::<Option<String>, _>("verdict").as_deref() {
                Some("approve") => AgentVerdict::Approve,
                Some("reject") => AgentVerdict::Reject,
                _ => AgentVerdict::Error,
            };
    }
    match LowCostRoute::from_verdicts(verdicts) {
        LowCostRoute::Approved => finalize(state, bundle, AuditDecision::ApprovedByLowCost).await?,
        LowCostRoute::EscalateHighCost => {
            sqlx::query("INSERT OR IGNORE INTO agent_runs(id, audit_bundle_sha256, tier, slot, attempt, adapter, model, adapter_version, prompt_version, status) VALUES (?, ?, 'high', 1, 0, 'unconfigured', 'unconfigured', 'v1', 'v1', 'pending')")
                .bind(Uuid::new_v4().to_string()).bind(bundle).execute(&state.database).await.map_err(ApiError::internal)?;
        }
        LowCostRoute::ManualReview => finalize(state, bundle, AuditDecision::ManualReview).await?,
    }
    Ok(())
}

async fn finalize(state: &AppState, bundle: &str, decision: AuditDecision) -> Result<(), ApiError> {
    let bundle_row = sqlx::query("SELECT revision_id, state FROM audit_bundles WHERE sha256 = ?")
        .bind(bundle)
        .fetch_one(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let revision_id: String = bundle_row.get("revision_id");
    if !matches!(
        bundle_row.get::<String, _>("state").as_str(),
        "agent_pending" | "agent_running"
    ) {
        return Ok(());
    }
    let report_hashes: Vec<String> = sqlx::query_scalar("SELECT report_sha256 FROM agent_runs WHERE audit_bundle_sha256 = ? AND report_sha256 IS NOT NULL ORDER BY tier, slot, attempt")
        .bind(bundle).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let report_sha = hex::encode(Sha256::digest(report_hashes.join("").as_bytes()));
    let (decision_name, bundle_state, revision_state) = match decision {
        AuditDecision::ApprovedByLowCost => ("approved_by_low_cost", "approved", "audit_approved"),
        AuditDecision::ApprovedByHighCost => {
            ("approved_by_high_cost", "approved", "audit_approved")
        }
        _ => ("manual_review", "manual_review", "audit_pending"),
    };
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    let updated = sqlx::query("UPDATE audit_bundles SET state = ? WHERE sha256 = ? AND state IN ('agent_pending', 'agent_running')")
        .bind(bundle_state)
        .bind(bundle)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    if updated.rows_affected() == 0 {
        transaction.rollback().await.map_err(ApiError::internal)?;
        return Ok(());
    }
    sqlx::query("UPDATE revisions SET state = ? WHERE id = ?")
        .bind(revision_state)
        .bind(&revision_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO audit_decisions(id, revision_id, audit_bundle_sha256, policy_version, decision, decided_by, rationale, report_sha256, created_at) VALUES (?, ?, ?, 'v1', ?, 'agent_orchestrator', NULL, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&revision_id).bind(bundle).bind(decision_name).bind(report_sha).bind(Utc::now())
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    if decision == AuditDecision::ManualReview {
        sqlx::query("INSERT INTO manual_actions(id, action_type, aggregate_type, aggregate_id, requested_by, state, details_json, created_at) VALUES (?, 'audit_decision', 'revision', ?, 'agent_orchestrator', 'pending', ?, ?)")
            .bind(Uuid::new_v4().to_string()).bind(&revision_id).bind(json!({"bundle_sha256": bundle}).to_string()).bind(Utc::now())
            .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    transaction.commit().await.map_err(ApiError::internal)?;
    if matches!(
        decision,
        AuditDecision::ApprovedByLowCost | AuditDecision::ApprovedByHighCost
    ) {
        crate::packages::schedule_ready_builds(&state.database).await?;
    }
    Ok(())
}

async fn low_cost_objections(state: &AppState, bundle: &str) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT report_json FROM agent_runs WHERE audit_bundle_sha256 = ? AND tier = 'low' AND report_json IS NOT NULL ORDER BY slot")
        .bind(bundle).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    rows.into_iter()
        .map(|value| {
            let report: Value = parse_json(&value)?;
            Ok(json!({
                "verdict": report.get("verdict"),
                "summary": report.get("summary"),
                "findings": report.get("findings"),
                "files_read": report.get("files_read")
            }))
        })
        .collect()
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT audit_bundles.sha256, audit_bundles.revision_id, audit_bundles.state, audit_bundles.policy_version, audit_bundles.deterministic_findings_json, audit_bundles.coverage_json, audit_bundles.created_at, revisions.package_base, revisions.aur_commit FROM audit_bundles JOIN revisions ON revisions.id = audit_bundles.revision_id ORDER BY audit_bundles.created_at DESC LIMIT 200")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "sha256": row.get::<String,_>("sha256"), "revision_id": row.get::<String,_>("revision_id"), "state": row.get::<String,_>("state"),
        "policy_version": row.get::<String,_>("policy_version"), "package_base": row.get::<String,_>("package_base"), "aur_commit": row.get::<String,_>("aur_commit"),
        "findings": parse_json::<Value>(row.get("deterministic_findings_json")).unwrap_or(Value::Null),
        "coverage": parse_json::<Value>(row.get("coverage_json")).unwrap_or(Value::Null), "created_at": row.get::<String,_>("created_at")
    })).collect::<Vec<_>>() })))
}

pub async fn manual_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(bundle): Path<String>,
    Json(request): Json<ManualDecisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    if request.rationale.trim().len() < 8 {
        return Err(ApiError::bad_request(
            "RATIONALE_REQUIRED",
            "人工审计理由至少 8 个字符",
        ));
    }
    let row = sqlx::query(
        "SELECT revision_id, policy_version, state FROM audit_bundles WHERE sha256 = ?",
    )
    .bind(&bundle)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("AuditBundle 不存在"))?;
    if row.get::<String, _>("state") != "manual_review" {
        return Err(ApiError::conflict(
            "AUDIT_NOT_MANUAL",
            "该审计当前不在人工队列",
        ));
    }
    let revision_id: String = row.get("revision_id");
    let decision = if request.approve {
        "manually_approved"
    } else {
        "manually_rejected"
    };
    let revision_state = if request.approve {
        "audit_approved"
    } else {
        "audit_rejected"
    };
    let report_sha = hex::encode(Sha256::digest(
        format!("{bundle}:{}:{}", request.approve, request.rationale).as_bytes(),
    ));
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE audit_bundles SET state = ? WHERE sha256 = ?")
        .bind(if request.approve {
            "approved"
        } else {
            "rejected"
        })
        .bind(&bundle)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("UPDATE revisions SET state = ? WHERE id = ?")
        .bind(revision_state)
        .bind(&revision_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO audit_decisions(id, revision_id, audit_bundle_sha256, policy_version, decision, decided_by, rationale, report_sha256, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&revision_id).bind(&bundle).bind(row.get::<String,_>("policy_version")).bind(decision).bind(&actor).bind(request.rationale.trim()).bind(report_sha).bind(Utc::now())
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE manual_actions SET state = 'completed', completed_at = ? WHERE aggregate_id = ? AND state = 'pending'").bind(Utc::now()).bind(&revision_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    if request.approve {
        crate::packages::schedule_ready_builds(&state.database).await?;
    }
    Ok(Json(json!({"bundle_sha256": bundle, "decision": decision})))
}

fn parse_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, ApiError> {
    serde_json::from_str(value).map_err(ApiError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_low_cost_vote_routing_is_not_overridden() {
        assert_eq!(
            LowCostRoute::from_verdicts([AgentVerdict::Approve; 3]),
            LowCostRoute::Approved
        );
        assert_eq!(
            LowCostRoute::from_verdicts([
                AgentVerdict::Approve,
                AgentVerdict::Approve,
                AgentVerdict::Error
            ]),
            LowCostRoute::EscalateHighCost
        );
        assert_eq!(
            LowCostRoute::from_verdicts([
                AgentVerdict::Approve,
                AgentVerdict::Reject,
                AgentVerdict::Error
            ]),
            LowCostRoute::ManualReview
        );
    }

    async fn fixture(verdicts: [&str; 3]) -> AppState {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        sqlx::query("INSERT INTO revisions(id, package_base, aur_commit, upstream_version, input_sha256, audit_policy_version, state, metadata_json, created_at) VALUES ('revision', 'demo', ?, '1-1', ?, 'v1', 'audit_pending', '{}', ?)")
            .bind("a".repeat(40)).bind("b".repeat(64)).bind(now).execute(&database).await.unwrap();
        sqlx::query("INSERT INTO audit_bundles(sha256, revision_id, policy_version, payload_json, coverage_json, deterministic_findings_json, state, created_at) VALUES (?, 'revision', 'v1', '{}', '{}', '[]', 'agent_running', ?)")
            .bind("c".repeat(64)).bind(now).execute(&database).await.unwrap();
        for (index, verdict) in verdicts.into_iter().enumerate() {
            sqlx::query("INSERT INTO agent_runs(id, audit_bundle_sha256, tier, slot, attempt, adapter, model, adapter_version, prompt_version, status, verdict, report_sha256) VALUES (?, ?, 'low', ?, 0, 'test', 'test', 'v1', 'v1', 'succeeded', ?, ?)")
                .bind(Uuid::new_v4().to_string()).bind("c".repeat(64)).bind(i64::try_from(index + 1).unwrap()).bind(verdict).bind(format!("{index:064x}"))
                .execute(&database).await.unwrap();
        }
        let config = crate::config::Config {
            bind_address: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            setup_token: "测试初始化令牌-至少二十个字符".into(),
            signing_key_file: "/不存在".into(),
            ssh_identity_source_file: "/不存在".into(),
            ssh_identity_file: "/不存在".into(),
            ssh_known_hosts_file: "/不存在".into(),
            secure_cookies: false,
            session_hours: 1,
            low_agent_endpoints: vec![],
            high_agent_endpoint: String::new(),
            agent_daily_call_limit: 300,
            agent_monthly_call_limit: 3000,
            agent_monthly_cost_limit_microusd: 5_000_000,
            repository_name: "aursmith".into(),
            source_git_commit: "test".into(),
            repository_base_url: "https://repo.test".into(),
            webhook_url: None,
            webhook_hmac_secret_file: "/不存在".into(),
            ntfy_url: None,
            backup_dir: "/不存在".into(),
            backup_export_dir: "/不存在".into(),
            backup_export_socket: "/不存在".into(),
        };
        AppState::new(
            database,
            config,
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        )
    }

    #[tokio::test]
    async fn three_approvals_finalize_without_high_cost_run() {
        let state = fixture(["approve", "approve", "approve"]).await;
        evaluate(&state, &"c".repeat(64), "low").await.unwrap();
        let bundle_state: String = sqlx::query_scalar("SELECT state FROM audit_bundles")
            .fetch_one(&state.database)
            .await
            .unwrap();
        let high_runs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE tier = 'high'")
                .fetch_one(&state.database)
                .await
                .unwrap();
        assert_eq!(bundle_state, "approved");
        assert_eq!(high_runs, 0);
    }

    #[tokio::test]
    async fn exactly_two_approvals_schedule_one_high_cost_run() {
        let state = fixture(["approve", "approve", "error"]).await;
        evaluate(&state, &"c".repeat(64), "low").await.unwrap();
        let high_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_runs WHERE tier = 'high' AND status = 'pending'",
        )
        .fetch_one(&state.database)
        .await
        .unwrap();
        assert_eq!(high_runs, 1);
    }

    #[tokio::test]
    async fn one_approval_enters_manual_queue_without_high_cost_run() {
        let state = fixture(["approve", "reject", "error"]).await;
        evaluate(&state, &"c".repeat(64), "low").await.unwrap();
        let manual: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM manual_actions WHERE state = 'pending'")
                .fetch_one(&state.database)
                .await
                .unwrap();
        let high_runs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE tier = 'high'")
                .fetch_one(&state.database)
                .await
                .unwrap();
        assert_eq!(manual, 1);
        assert_eq!(high_runs, 0);
    }

    #[tokio::test]
    async fn automatic_finalization_is_idempotent() {
        let state = fixture(["approve", "reject", "error"]).await;
        let bundle = "c".repeat(64);
        evaluate(&state, &bundle, "low").await.unwrap();
        evaluate(&state, &bundle, "low").await.unwrap();
        let decisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_decisions")
            .fetch_one(&state.database)
            .await
            .unwrap();
        let actions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM manual_actions")
            .fetch_one(&state.database)
            .await
            .unwrap();
        assert_eq!(decisions, 1);
        assert_eq!(actions, 1);
    }

    #[tokio::test]
    async fn exhausted_budget_is_not_available() {
        let state = fixture(["approve", "approve", "approve"]).await;
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('agent_daily_call_limit', '0', ?)")
            .bind(Utc::now()).execute(&state.database).await.unwrap();
        assert!(!budget_available(&state).await.unwrap());
    }
}
