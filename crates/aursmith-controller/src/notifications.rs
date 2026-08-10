use crate::{error::ApiError, routes::AppState};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use std::path::Path;

pub async fn dispatch_one(state: &AppState) -> Result<(), ApiError> {
    for (channel, enabled) in [
        ("webhook", state.config.webhook_url.is_some()),
        ("ntfy", state.config.ntfy_url.is_some()),
    ] {
        if enabled {
            sqlx::query("INSERT INTO alert_notifications(id, alert_id, alert_state, channel, status, created_at) SELECT lower(hex(randomblob(16))), alerts.id, alerts.state, ?, 'pending', ? FROM alerts WHERE NOT EXISTS (SELECT 1 FROM alert_notifications WHERE alert_notifications.alert_id = alerts.id AND alert_notifications.alert_state = alerts.state AND alert_notifications.channel = ?)")
                .bind(channel).bind(Utc::now()).bind(channel).execute(&state.database).await.map_err(ApiError::internal)?;
        }
    }
    let row = sqlx::query("SELECT alert_notifications.id, alert_notifications.channel, alert_notifications.attempt_count, alerts.id AS alert_id, alerts.fingerprint, alerts.severity, alerts.state, alerts.title, alerts.details_json, alerts.opened_at FROM alert_notifications JOIN alerts ON alerts.id = alert_notifications.alert_id WHERE alert_notifications.status = 'pending' ORDER BY alert_notifications.created_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(row) = row else {
        return Ok(());
    };
    let notification_id: String = row.get("id");
    let channel: String = row.get("channel");
    let details = serde_json::from_str::<Value>(row.get("details_json")).unwrap_or(Value::Null);
    let payload = json!({
        "schema_version": 1,
        "event": "alert_state_changed",
        "alert": {
            "id": row.get::<String,_>("alert_id"),
            "fingerprint": row.get::<String,_>("fingerprint"),
            "severity": row.get::<String,_>("severity"),
            "state": row.get::<String,_>("state"),
            "title": row.get::<String,_>("title"),
            "details": details,
            "opened_at": row.get::<String,_>("opened_at"),
        },
        "sent_at": Utc::now(),
    });
    let result = if channel == "webhook" {
        send_webhook(state, &payload).await
    } else {
        send_ntfy(state, &payload).await
    };
    match result {
        Ok(()) => {
            sqlx::query("UPDATE alert_notifications SET status = 'delivered', delivered_at = ?, last_error = NULL WHERE id = ?")
                .bind(Utc::now()).bind(notification_id).execute(&state.database).await.map_err(ApiError::internal)?;
        }
        Err(error) => {
            let attempts: i64 = row.get("attempt_count");
            sqlx::query("UPDATE alert_notifications SET status = CASE WHEN attempt_count + 1 >= 3 THEN 'failed' ELSE 'pending' END, attempt_count = attempt_count + 1, last_error = ? WHERE id = ?")
                .bind(error.to_string()).bind(notification_id).execute(&state.database).await.map_err(ApiError::internal)?;
            if attempts + 1 >= 3 {
                tracing::warn!(%channel, %error, "告警通知三次投递失败");
            }
        }
    }
    Ok(())
}

async fn send_webhook(state: &AppState, payload: &Value) -> anyhow::Result<()> {
    let url = state
        .config
        .webhook_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Webhook 未配置"))?;
    validate_url(url)?;
    let path = Path::new(&state.config.webhook_hmac_secret_file);
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() < 16 || metadata.len() > 4096 {
        anyhow::bail!("Webhook HMAC secret 类型或长度无效");
    }
    let secret = std::fs::read(path)?;
    let bytes = serde_json::to_vec(payload)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&secret)?;
    mac.update(&bytes);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    reqwest::Client::new()
        .post(url)
        .header("X-AURsmith-Signature", signature)
        .header("Content-Type", "application/json")
        .body(bytes)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn send_ntfy(state: &AppState, payload: &Value) -> anyhow::Result<()> {
    let url = state
        .config
        .ntfy_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("ntfy 未配置"))?;
    validate_url(url)?;
    let alert = &payload["alert"];
    reqwest::Client::new()
        .post(url)
        .header("Title", alert["title"].as_str().unwrap_or("AURsmith 告警"))
        .header(
            "Priority",
            if alert["severity"].as_str() == Some("critical") {
                "urgent"
            } else {
                "default"
            },
        )
        .body(serde_json::to_string_pretty(payload)?)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

fn validate_url(value: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        anyhow::bail!("通知 URL 无效");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_url_rejects_embedded_credentials() {
        assert!(validate_url("https://notify.example/topic").is_ok());
        assert!(validate_url("https://user:secret@notify.example/topic").is_err());
        assert!(validate_url("file:///tmp/socket").is_err());
    }
}
