use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVerdict {
    Approve,
    Reject,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowCostRoute {
    Approved,
    EscalateHighCost,
    ManualReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    ApprovedByLowCost,
    ApprovedByHighCost,
    ManualReview,
    BlockedDeterministically,
}

impl LowCostRoute {
    /// 精确实现 A02。拒绝和调用错误都不能计为通过；重试由编排器负责，
    /// 且必须在调用此纯决策函数之前结束。
    pub fn from_verdicts(verdicts: [AgentVerdict; 3]) -> Self {
        match verdicts
            .iter()
            .filter(|verdict| **verdict == AgentVerdict::Approve)
            .count()
        {
            3 => Self::Approved,
            2 => Self::EscalateHighCost,
            _ => Self::ManualReview,
        }
    }

    pub fn finalize(self, high_cost: Option<AgentVerdict>) -> AuditDecision {
        match self {
            Self::Approved => AuditDecision::ApprovedByLowCost,
            Self::EscalateHighCost if high_cost == Some(AgentVerdict::Approve) => {
                AuditDecision::ApprovedByHighCost
            }
            Self::EscalateHighCost | Self::ManualReview => AuditDecision::ManualReview,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_approvals_pass_without_escalation() {
        let route = LowCostRoute::from_verdicts([AgentVerdict::Approve; 3]);
        assert_eq!(route, LowCostRoute::Approved);
        assert_eq!(route.finalize(None), AuditDecision::ApprovedByLowCost);
    }

    #[test]
    fn exactly_two_approvals_escalate() {
        let route = LowCostRoute::from_verdicts([
            AgentVerdict::Approve,
            AgentVerdict::Error,
            AgentVerdict::Approve,
        ]);
        assert_eq!(route, LowCostRoute::EscalateHighCost);
        assert_eq!(
            route.finalize(Some(AgentVerdict::Approve)),
            AuditDecision::ApprovedByHighCost
        );
        assert_eq!(
            route.finalize(Some(AgentVerdict::Reject)),
            AuditDecision::ManualReview
        );
    }

    #[test]
    fn one_or_fewer_approvals_require_manual_review() {
        let route = LowCostRoute::from_verdicts([
            AgentVerdict::Approve,
            AgentVerdict::Reject,
            AgentVerdict::Error,
        ]);
        assert_eq!(route, LowCostRoute::ManualReview);
        assert_eq!(
            route.finalize(Some(AgentVerdict::Approve)),
            AuditDecision::ManualReview
        );
    }
}
