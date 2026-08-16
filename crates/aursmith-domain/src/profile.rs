use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePolicy {
    pub minimum_observations: u32,
    pub recent_window: u32,
    pub minimum_recent_percent: u8,
    pub minimum_monthly_uses: u32,
    pub minimum_saved_seconds: u64,
    pub add_periods: u8,
    pub remove_periods: u8,
    pub remove_after_unused_days: u32,
    pub maximum_active_profiles: u8,
}

impl Default for ProfilePolicy {
    fn default() -> Self {
        Self {
            minimum_observations: 20,
            recent_window: 20,
            minimum_recent_percent: 30,
            minimum_monthly_uses: 5,
            minimum_saved_seconds: 60,
            add_periods: 2,
            remove_periods: 3,
            remove_after_unused_days: 30,
            maximum_active_profiles: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyStats {
    pub total_observations: u32,
    pub uses_in_recent_window: u32,
    pub uses_this_month: u32,
    pub estimated_saved_seconds: u64,
    pub consecutive_add_periods: u8,
    pub consecutive_low_periods: u8,
    pub days_since_last_use: u32,
    pub currently_baked: bool,
    pub official_repository_package: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyAction {
    ObserveOnly,
    SuggestAdd,
    Add,
    Keep,
    SuggestRemove,
    Remove,
    IgnoreAurDependency,
}

impl ProfilePolicy {
    pub fn evaluate(&self, stats: DependencyStats) -> DependencyAction {
        if !stats.official_repository_package {
            return DependencyAction::IgnoreAurDependency;
        }
        if stats.total_observations < self.minimum_observations {
            return DependencyAction::ObserveOnly;
        }

        let recent_percent = if self.recent_window == 0 {
            0
        } else {
            stats.uses_in_recent_window.saturating_mul(100) / self.recent_window
        };
        let is_hot = (recent_percent >= u32::from(self.minimum_recent_percent)
            || stats.uses_this_month >= self.minimum_monthly_uses)
            && stats.estimated_saved_seconds >= self.minimum_saved_seconds;

        if stats.currently_baked {
            if stats.consecutive_low_periods >= self.remove_periods
                || stats.days_since_last_use >= self.remove_after_unused_days
            {
                DependencyAction::Remove
            } else if !is_hot {
                DependencyAction::SuggestRemove
            } else {
                DependencyAction::Keep
            }
        } else if is_hot && stats.consecutive_add_periods >= self.add_periods {
            DependencyAction::Add
        } else if is_hot {
            DependencyAction::SuggestAdd
        } else {
            DependencyAction::ObserveOnly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> DependencyStats {
        DependencyStats {
            total_observations: 20,
            uses_in_recent_window: 6,
            uses_this_month: 5,
            estimated_saved_seconds: 60,
            consecutive_add_periods: 2,
            consecutive_low_periods: 0,
            days_since_last_use: 0,
            currently_baked: false,
            official_repository_package: true,
        }
    }

    #[test]
    fn first_twenty_builds_are_observation_only() {
        let mut value = stats();
        value.total_observations = 19;
        assert_eq!(
            ProfilePolicy::default().evaluate(value),
            DependencyAction::ObserveOnly
        );
    }

    #[test]
    fn hot_dependency_needs_two_periods_before_addition() {
        let mut value = stats();
        value.consecutive_add_periods = 1;
        assert_eq!(
            ProfilePolicy::default().evaluate(value),
            DependencyAction::SuggestAdd
        );
        value.consecutive_add_periods = 2;
        assert_eq!(
            ProfilePolicy::default().evaluate(value),
            DependencyAction::Add
        );
    }

    #[test]
    fn aur_dependencies_are_never_baked() {
        let mut value = stats();
        value.official_repository_package = false;
        assert_eq!(
            ProfilePolicy::default().evaluate(value),
            DependencyAction::IgnoreAurDependency
        );
    }

    #[test]
    fn unused_baked_dependency_is_removed_after_thirty_days() {
        let mut value = stats();
        value.currently_baked = true;
        value.uses_in_recent_window = 0;
        value.uses_this_month = 0;
        value.days_since_last_use = 30;
        assert_eq!(
            ProfilePolicy::default().evaluate(value),
            DependencyAction::Remove
        );
    }
}
