//! 状态导出 / 导入 / 旧库转换。
//!
//! - `aursmithd export`：把当前库（会话除外）导出为带版本的 JSON，作为升级或迁移前的安全备份。
//! - `aursmithd legacy-export`：只读打开旧版（27 表）数据库，转换为同一 JSON 格式。
//! - `aursmithd import`：导入到一个**空的**新库；在同一事务内重新导出并逐表比对，
//!   不一致则回滚，保证导入结果可验证。

use crate::{
    aur::{Snapshot, SnapshotFile},
    packages::ResolvedDependency,
};
use anyhow::{Context, bail};
use aursmith_core::srcinfo::parse_srcinfo;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{collections::BTreeMap, str::FromStr};

pub const STATE_FORMAT: &str = "aursmith-state";
pub const STATE_VERSION: u32 = 2;

/// 按外键依赖排列的业务表（不含 sessions：会话不迁移，迁移后需要重新登录）。
pub const TABLES: &[&str] = &[
    "admin",
    "packages",
    "subscriptions",
    "provider_choices",
    "revisions",
    "reviews",
    "builds",
    "publications",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDocument {
    pub format: String,
    pub version: u32,
    pub source: String,
    pub exported_at: String,
    pub tables: BTreeMap<String, Vec<Map<String, Value>>>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl StateDocument {
    pub fn counts(&self) -> BTreeMap<String, usize> {
        self.tables
            .iter()
            .map(|(name, rows)| (name.clone(), rows.len()))
            .collect()
    }

    pub fn tables_sha256(&self) -> String {
        aursmith_core::sha256_hex(&serde_json::to_vec(&self.tables).expect("表内容可以序列化"))
    }
}

async fn columns(connection: &mut SqliteConnection, table: &str) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

async fn export_tables(
    connection: &mut SqliteConnection,
) -> anyhow::Result<BTreeMap<String, Vec<Map<String, Value>>>> {
    let mut tables = BTreeMap::new();
    for table in TABLES {
        let columns = columns(connection, table).await?;
        let pairs = columns
            .iter()
            .map(|column| format!("'{column}', \"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let key = match *table {
            "admin" => "id",
            "packages" | "subscriptions" => "package_base",
            "provider_choices" => "package_base, dependency",
            _ => "created_at, id",
        };
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT json_object({pairs}) FROM {table} ORDER BY {key}"
        ))
        .fetch_all(&mut *connection)
        .await?;
        let rows = rows
            .into_iter()
            .map(|row| serde_json::from_str::<Map<String, Value>>(&row))
            .collect::<Result<Vec<_>, _>>()?;
        tables.insert((*table).to_owned(), rows);
    }
    Ok(tables)
}

pub async fn export(connection: &mut SqliteConnection) -> anyhow::Result<StateDocument> {
    Ok(StateDocument {
        format: STATE_FORMAT.into(),
        version: STATE_VERSION,
        source: "aursmithd".into(),
        exported_at: Utc::now().to_rfc3339(),
        tables: export_tables(connection).await?,
        warnings: Vec::new(),
    })
}

/// 导入到空库，并在提交前重新导出逐表比对。
pub async fn import(
    connection: &mut SqliteConnection,
    document: &StateDocument,
) -> anyhow::Result<BTreeMap<String, usize>> {
    if document.format != STATE_FORMAT || document.version != STATE_VERSION {
        bail!(
            "不支持的状态文件格式 {} v{}",
            document.format,
            document.version
        );
    }
    for name in document.tables.keys() {
        if !TABLES.contains(&name.as_str()) {
            bail!("状态文件包含未知表 {name}");
        }
    }
    for table in TABLES {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut *connection)
            .await?;
        if count > 0 {
            bail!("目标数据库不是空库（{table} 已有 {count} 行），拒绝导入");
        }
    }
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await?;
    let mut transaction = connection.begin().await?;
    for table in TABLES {
        let allowed = columns(&mut transaction, table).await?;
        for row in document.tables.get(*table).into_iter().flatten() {
            for column in row.keys() {
                if !allowed.contains(column) {
                    bail!("{table} 包含未知列 {column}");
                }
            }
            let names: Vec<&String> = row.keys().collect();
            let sql = format!(
                "INSERT INTO {table}({}) VALUES ({})",
                names
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
                vec!["?"; names.len()].join(", ")
            );
            let mut query = sqlx::query(&sql);
            for name in &names {
                query = match &row[*name] {
                    Value::Null => query.bind(None::<String>),
                    Value::Bool(value) => query.bind(i64::from(*value)),
                    Value::Number(value) => match value.as_i64() {
                        Some(integer) => query.bind(integer),
                        None => query.bind(value.as_f64()),
                    },
                    Value::String(value) => query.bind(value.clone()),
                    other => query.bind(other.to_string()),
                };
            }
            query
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("导入 {table} 失败"))?;
        }
    }
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *transaction)
        .await?;
    if !violations.is_empty() {
        bail!("导入后存在 {} 处外键不一致", violations.len());
    }
    let reexported = normalize(export_tables(&mut transaction).await?);
    let mut expected = normalize(document.tables.clone());
    for table in TABLES {
        expected.entry((*table).to_owned()).or_default();
    }
    if reexported != expected {
        let differing: Vec<&String> = expected
            .keys()
            .filter(|table| reexported.get(*table) != expected.get(*table))
            .collect();
        bail!("导入校验失败，以下表重新导出后不一致：{differing:?}");
    }
    transaction.commit().await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await?;
    Ok(expected
        .iter()
        .map(|(name, rows)| (name.clone(), rows.len()))
        .collect())
}

/// 行顺序无关的比较形式。
fn normalize(
    mut tables: BTreeMap<String, Vec<Map<String, Value>>>,
) -> BTreeMap<String, Vec<Map<String, Value>>> {
    for rows in tables.values_mut() {
        rows.sort_by_cached_key(|row| serde_json::to_string(row).unwrap_or_default());
    }
    tables
}

/// 比较数据库当前内容与状态文件（行顺序无关）。
pub async fn verify(
    connection: &mut SqliteConnection,
    document: &StateDocument,
) -> anyhow::Result<BTreeMap<String, usize>> {
    let actual = normalize(export_tables(connection).await?);
    let mut expected = normalize(document.tables.clone());
    for table in TABLES {
        expected.entry((*table).to_owned()).or_default();
    }
    if actual != expected {
        let differing: Vec<&String> = expected
            .keys()
            .filter(|table| actual.get(*table) != expected.get(*table))
            .collect();
        bail!("数据库内容与状态文件不一致：{differing:?}");
    }
    Ok(expected
        .iter()
        .map(|(name, rows)| (name.clone(), rows.len()))
        .collect())
}

// ---------------------------------------------------------------- 旧库转换

#[derive(Deserialize)]
struct LegacySnapshotFile {
    path: String,
    sha256: String,
    size: u64,
    binary: bool,
    content_base64: String,
}

#[derive(Deserialize)]
struct LegacySnapshot {
    package_base: String,
    aur_commit: String,
    vcs_commit: Option<String>,
    srcinfo: String,
    files: Vec<LegacySnapshotFile>,
}

fn legacy_state(provider_state: &str) -> &'static str {
    match provider_state {
        "resolved" => "aur",
        "needs_selection" => "needs_selection",
        "official_or_unknown" => "official",
        _ => "unknown",
    }
}

/// 只读打开旧库，转换为 v2 状态文档。不迁移构建与发布历史：导入后调和循环会为每个
/// 已批准 revision 重新构建并发布一次（旧仓库在新发布完成前保持可用）。
pub async fn legacy_export(legacy_database_url: &str) -> anyhow::Result<StateDocument> {
    let options = SqliteConnectOptions::from_str(legacy_database_url)?.read_only(true);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    let mut warnings = Vec::new();
    let mut tables: BTreeMap<String, Vec<Map<String, Value>>> = BTreeMap::new();
    let object = |value: Value| value.as_object().cloned().expect("json! 对象");
    let now = Utc::now().to_rfc3339();

    let admin = sqlx::query("SELECT username, password_hash, created_at FROM administrators ORDER BY created_at LIMIT 1")
        .fetch_optional(&mut connection)
        .await?;
    tables.insert(
        "admin".into(),
        admin
            .map(|row| {
                vec![object(json!({
                    "id": 1,
                    "username": row.get::<String, _>("username"),
                    "password_hash": row.get::<String, _>("password_hash"),
                    "updated_at": row.get::<String, _>("created_at"),
                }))]
            })
            .unwrap_or_default(),
    );

    let mut packages: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for row in sqlx::query("SELECT package_bases.name, version, description, maintainer, out_of_date_at, aur_last_modified, outputs_json, COALESCE(package_build_policies.allow_check, 1) AS allow_check, package_sync_state.last_checked_at FROM package_bases LEFT JOIN package_build_policies ON package_build_policies.package_base = package_bases.name LEFT JOIN package_sync_state ON package_sync_state.package_base = package_bases.name")
        .fetch_all(&mut connection)
        .await?
    {
        let name: String = row.get("name");
        packages.insert(name.clone(), object(json!({
            "package_base": name,
            "version": row.get::<Option<String>, _>("version"),
            "description": row.get::<Option<String>, _>("description"),
            "maintainer": row.get::<Option<String>, _>("maintainer"),
            "out_of_date": row.get::<Option<i64>, _>("out_of_date_at"),
            "last_modified": row.get::<Option<i64>, _>("aur_last_modified"),
            "outputs_json": row.get::<String, _>("outputs_json"),
            "allow_check": row.get::<i64, _>("allow_check"),
            "next_check_at": now,
            "last_checked_at": row.get::<Option<String>, _>("last_checked_at"),
            "sync_failures": 0,
            "sync_error": null,
        })));
    }

    let mut subscriptions = Vec::new();
    let mut choices = Vec::new();
    for row in sqlx::query("SELECT package_base, selected_providers_json, created_at, updated_at FROM subscriptions WHERE kind = 'direct' ORDER BY package_base")
        .fetch_all(&mut connection)
        .await?
    {
        let base: String = row.get("package_base");
        subscriptions.push(object(json!({"package_base": base, "created_at": row.get::<String, _>("created_at")})));
        let selected: BTreeMap<String, String> = serde_json::from_str(row.get("selected_providers_json")).unwrap_or_else(|_| {
            warnings.push(format!("{base} 的 provider 选择无法解析，已忽略"));
            BTreeMap::new()
        });
        for (dependency, provider) in selected {
            choices.push(object(json!({
                "package_base": base,
                "dependency": dependency,
                "provider_base": provider,
                "updated_at": row.get::<String, _>("updated_at"),
            })));
        }
    }

    let approved = sqlx::query(
        "SELECT revisions.id, revisions.package_base, revisions.metadata_json, revisions.created_at, decision.decision, decision.decided_by, decision.rationale, decision.created_at AS decided_at \
         FROM revisions JOIN audit_decisions AS decision ON decision.revision_id = revisions.id \
         WHERE decision.decision IN ('approved_by_low_cost', 'approved_by_high_cost', 'manually_approved') \
         ORDER BY revisions.package_base, revisions.created_at DESC",
    )
    .fetch_all(&mut connection)
    .await?;
    let mut revisions = Vec::new();
    let mut reviews = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in approved {
        let base: String = row.get("package_base");
        if !seen.insert(base.clone()) {
            continue;
        }
        let revision_id: String = row.get("id");
        let legacy: LegacySnapshot = match serde_json::from_str(row.get("metadata_json")) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warnings.push(format!("{base} 的已批准 revision {revision_id} 快照无法解析（{error}），将作为首次审查重新处理"));
                continue;
            }
        };
        let info = match parse_srcinfo(&legacy.package_base, &legacy.srcinfo) {
            Ok(info) => info,
            Err(error) => {
                warnings.push(format!(
                    "{base} 的 .SRCINFO 无法解析（{error}），将作为首次审查重新处理"
                ));
                continue;
            }
        };
        let snapshot = Snapshot {
            package_base: legacy.package_base,
            aur_commit: legacy.aur_commit,
            vcs_commit: legacy.vcs_commit,
            info,
            files: legacy
                .files
                .into_iter()
                .map(|file| SnapshotFile {
                    path: file.path,
                    sha256: file.sha256,
                    size: file.size,
                    binary: file.binary,
                    content_base64: file.content_base64,
                })
                .collect(),
        };
        let dependencies: Vec<ResolvedDependency> = sqlx::query("SELECT dependency_name, dependency_kind, target_package_base, provider_state, candidates_json FROM revision_dependencies WHERE revision_id = ? ORDER BY dependency_name, dependency_kind")
            .bind(&revision_id)
            .fetch_all(&mut connection)
            .await?
            .into_iter()
            .map(|dependency| ResolvedDependency {
                name: dependency.get("dependency_name"),
                kind: dependency.get("dependency_kind"),
                state: legacy_state(dependency.get::<String, _>("provider_state").as_str()).to_owned(),
                target: dependency.get("target_package_base"),
                candidates: serde_json::from_str(dependency.get("candidates_json")).unwrap_or_default(),
            })
            .collect();
        packages.entry(base.clone()).or_insert_with(|| {
            object(json!({
                "package_base": base,
                "version": snapshot.info.version,
                "description": null,
                "maintainer": null,
                "out_of_date": null,
                "last_modified": null,
                "outputs_json": serde_json::to_string(&snapshot.info.outputs).unwrap_or_default(),
                "allow_check": 1,
                "next_check_at": now,
                "last_checked_at": null,
                "sync_failures": 0,
                "sync_error": null,
            }))
        });
        for target in dependencies
            .iter()
            .filter_map(|dependency| dependency.target.clone())
        {
            packages.entry(target.clone()).or_insert_with(|| object(json!({
                "package_base": target, "version": null, "description": null, "maintainer": null,
                "out_of_date": null, "last_modified": null, "outputs_json": "[]", "allow_check": 1,
                "next_check_at": now, "last_checked_at": null, "sync_failures": 0, "sync_error": null,
            })));
        }
        let decision: String = row.get("decision");
        let decided_at: String = row.get("decided_at");
        revisions.push(object(json!({
            "id": revision_id,
            "package_base": base,
            "aur_commit": snapshot.aur_commit,
            "vcs_commit": snapshot.vcs_commit,
            "version": snapshot.info.version,
            "tree_sha256": snapshot.tree_sha256(),
            "snapshot_json": serde_json::to_string(&snapshot)?,
            "dependencies_json": serde_json::to_string(&dependencies)?,
            "baseline_revision_id": null,
            "state": "approved",
            "created_at": row.get::<String, _>("created_at"),
            "decided_at": decided_at,
        })));
        let (kind, role) = match decision.as_str() {
            "manually_approved" => ("human", Value::Null),
            "approved_by_high_cost" => ("agent", json!("high")),
            _ => ("agent", json!("low")),
        };
        reviews.push(object(json!({
            "id": format!("legacy-{revision_id}"),
            "revision_id": revision_id,
            "kind": kind,
            "role": role,
            "model": if kind == "agent" { json!("legacy") } else { Value::Null },
            "verdict": "approve",
            "summary": row.get::<Option<String>, _>("rationale").unwrap_or_else(|| format!("旧系统审查结论：{decision}")),
            "findings_json": "[]",
            "created_at": decided_at,
        })));
    }
    tables.insert("packages".into(), packages.into_values().collect());
    tables.insert("subscriptions".into(), subscriptions);
    tables.insert("provider_choices".into(), choices);
    tables.insert("revisions".into(), revisions);
    tables.insert("reviews".into(), reviews);
    tables.insert("builds".into(), Vec::new());
    tables.insert("publications".into(), Vec::new());
    Ok(StateDocument {
        format: STATE_FORMAT.into(),
        version: STATE_VERSION,
        source: "legacy".into(),
        exported_at: now,
        tables,
        warnings,
    })
}
