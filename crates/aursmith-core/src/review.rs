//! 2+1 Agent 审查规则（纯函数）与 Agent 结构化输出的严格解析。
//!
//! 规则：
//! 1. 两个独立的低成本 Agent 并行审查同一份输入。
//! 2. 两个都返回合法 JSON 且都是 `approve` → 通过。
//! 3. 任一低成本 Agent `reject`、两者意见不一致、或任一返回非法/非结构化输出（`error`）
//!    → 调用一个高成本 Agent。
//! 4. 高成本 Agent `approve` → 通过；`reject` 或 `error` → 进入人工审查（manual_review）。
//! 5. 首次加入的软件包（没有已批准 baseline）无论 Agent 结果如何都进入人工审查，
//!    Agent 结论只作为参考。
//! 6. 确定性扫描出现 Block 级发现时直接拒绝，不调用 Agent。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    Reject,
    Error,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Error => "error",
        }
    }
}

/// 两个低成本 Agent 之后的下一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowStage {
    Approved,
    EscalateHigh,
}

pub fn after_lows(first: Verdict, second: Verdict) -> LowStage {
    if first == Verdict::Approve && second == Verdict::Approve {
        LowStage::Approved
    } else {
        LowStage::EscalateHigh
    }
}

/// 审查最终去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Approved,
    ManualReview,
    Rejected,
}

/// 根据 Agent 结论计算最终去向。`high` 只有在 `after_lows` 要求升级时才应该存在；
/// 需要升级却没有高成本结论时视为 error。
pub fn decide(first_time: bool, lows: [Verdict; 2], high: Option<Verdict>) -> Outcome {
    let agents_approve = match after_lows(lows[0], lows[1]) {
        LowStage::Approved => true,
        LowStage::EscalateHigh => high.unwrap_or(Verdict::Error) == Verdict::Approve,
    };
    if agents_approve && !first_time {
        Outcome::Approved
    } else {
        Outcome::ManualReview
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFinding {
    pub severity: Severity,
    pub category: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportVerdict {
    Approve,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub verdict: ReportVerdict,
    pub summary: String,
    pub findings: Vec<AgentFinding>,
    pub files_read: Vec<String>,
}

impl AgentReport {
    pub fn verdict(&self) -> Verdict {
        match self.verdict {
            ReportVerdict::Approve => Verdict::Approve,
            ReportVerdict::Reject => Verdict::Reject,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReportError {
    #[error("Agent 输出不是严格 JSON：{0}")]
    Json(String),
    #[error("Agent 输出超过大小限制")]
    TooLarge,
    #[error("Agent 输出自相矛盾：approve 但包含 critical 发现")]
    Inconsistent,
}

pub const MAXIMUM_REPORT_BYTES: usize = 256 * 1024;
pub const MAXIMUM_FINDINGS: usize = 100;

/// 严格解析 Agent 输出：只接受与 [`report_schema`] 一致的 JSON 对象，
/// 拒绝未知字段、Markdown 包裹、超限与自相矛盾的结论。解析失败按 `error` 处理。
pub fn parse_report(text: &str) -> Result<AgentReport, ReportError> {
    if text.len() > MAXIMUM_REPORT_BYTES {
        return Err(ReportError::TooLarge);
    }
    let report: AgentReport =
        serde_json::from_str(text.trim()).map_err(|error| ReportError::Json(error.to_string()))?;
    if report.findings.len() > MAXIMUM_FINDINGS || report.files_read.len() > 1024 {
        return Err(ReportError::TooLarge);
    }
    if report.verdict == ReportVerdict::Approve
        && report
            .findings
            .iter()
            .any(|finding| finding.severity == Severity::Critical)
    {
        return Err(ReportError::Inconsistent);
    }
    Ok(report)
}

/// 发给模型的 JSON Schema（OpenAI `json_schema` strict 模式 / Anthropic 工具入参）。
pub fn report_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "verdict": {"type": "string", "enum": ["approve", "reject"]},
            "summary": {"type": "string"},
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "severity": {"type": "string", "enum": ["info", "warning", "high", "critical"]},
                        "category": {"type": "string"},
                        "message": {"type": "string"},
                        "file": {"type": ["string", "null"]},
                        "line": {"type": ["integer", "null"]},
                        "evidence": {"type": "string"}
                    },
                    "required": ["severity", "category", "message", "file", "line", "evidence"]
                }
            },
            "files_read": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["verdict", "summary", "findings", "files_read"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Verdict::{Approve, Error, Reject};

    #[test]
    fn both_lows_approve_passes_without_high() {
        assert_eq!(after_lows(Approve, Approve), LowStage::Approved);
        assert_eq!(decide(false, [Approve, Approve], None), Outcome::Approved);
    }

    #[test]
    fn any_reject_disagreement_or_error_escalates() {
        for lows in [
            [Approve, Reject],
            [Reject, Approve],
            [Reject, Reject],
            [Approve, Error],
            [Error, Error],
        ] {
            assert_eq!(
                after_lows(lows[0], lows[1]),
                LowStage::EscalateHigh,
                "{lows:?}"
            );
        }
    }

    #[test]
    fn high_decides_after_escalation() {
        assert_eq!(
            decide(false, [Approve, Reject], Some(Approve)),
            Outcome::Approved
        );
        assert_eq!(
            decide(false, [Approve, Reject], Some(Reject)),
            Outcome::ManualReview
        );
        assert_eq!(
            decide(false, [Error, Approve], Some(Error)),
            Outcome::ManualReview
        );
        assert_eq!(decide(false, [Error, Approve], None), Outcome::ManualReview);
    }

    #[test]
    fn first_time_packages_always_need_a_human() {
        assert_eq!(
            decide(true, [Approve, Approve], None),
            Outcome::ManualReview
        );
        assert_eq!(
            decide(true, [Reject, Approve], Some(Approve)),
            Outcome::ManualReview
        );
    }

    #[test]
    fn strict_report_parsing() {
        let ok = r#"{"verdict":"approve","summary":"ok","findings":[],"files_read":["PKGBUILD"]}"#;
        assert_eq!(parse_report(ok).unwrap().verdict(), Approve);
        let unknown =
            r#"{"verdict":"approve","summary":"ok","findings":[],"files_read":[],"extra":1}"#;
        assert!(parse_report(unknown).is_err());
        assert!(parse_report("```json\n{}\n```").is_err());
        let bad_enum = r#"{"verdict":"maybe","summary":"","findings":[],"files_read":[]}"#;
        assert!(parse_report(bad_enum).is_err());
        let inconsistent = r#"{"verdict":"approve","summary":"","files_read":[],"findings":[{"severity":"critical","category":"x","message":"m","file":null,"line":null,"evidence":"e"}]}"#;
        assert_eq!(parse_report(inconsistent), Err(ReportError::Inconsistent));
    }

    #[test]
    fn schema_requires_every_report_field() {
        let schema = report_schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 4);
        assert_eq!(schema["additionalProperties"], false);
    }
}
