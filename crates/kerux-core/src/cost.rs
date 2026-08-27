//! Cost guardrails and budget enforcement (Ide #10).
//!
//! Provides budget limits (per-run and daily), auto-pause triggers,
//! threshold notifications, and auto-downgrade to cheaper models.

use crate::config::BudgetSettings;
use serde::{Deserialize, Serialize};

/// Action to take on budget limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    #[default]
    Pause,
    Downgrade,
    Stop,
}

/// Evaluation outcome from cost guardrail checks.
#[derive(Debug, Clone, PartialEq)]
pub enum GuardrailVerdict {
    /// Within safe budget margins.
    Ok,
    /// Approaching budget limit; threshold reached.
    Warn {
        current_run_cost: f64,
        daily_cost: f64,
        downgrade_model: Option<String>,
        message: String,
    },
    /// Hard budget ceiling exceeded; execution must pause or stop or downgrade.
    LimitExceeded {
        action: BudgetAction,
        reason: String,
        current_run_cost: f64,
        daily_cost: f64,
        downgrade_model: Option<String>,
    },
}

/// Cost tracker and budget evaluator.
#[derive(Debug, Clone, Default)]
pub struct CostGuardrail {
    pub budget: BudgetSettings,
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
    pub accumulated_day_cost: f64,
    pub current_run_cost: f64,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    /// Set once a soft warning has been emitted for the current run, so the
    /// warn threshold notifies exactly once instead of every turn.
    pub warn_emitted: bool,
    /// Set once a downgrade has been applied for the current run, so the
    /// downgrade action fires exactly once instead of every turn.
    pub downgrade_applied: bool,
}

impl CostGuardrail {
    pub fn new(
        budget: BudgetSettings,
        input_cost_per_million: f64,
        output_cost_per_million: f64,
    ) -> Self {
        Self {
            budget,
            input_cost_per_million,
            output_cost_per_million,
            accumulated_day_cost: 0.0,
            current_run_cost: 0.0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            warn_emitted: false,
            downgrade_applied: false,
        }
    }

    /// Calculate cost for a given token usage.
    pub fn calculate_cost(&self, prompt_tokens: usize, completion_tokens: usize) -> f64 {
        let input_cost = (prompt_tokens as f64 / 1_000_000.0) * self.input_cost_per_million;
        let output_cost = (completion_tokens as f64 / 1_000_000.0) * self.output_cost_per_million;
        input_cost + output_cost
    }

    /// Record token usage for the active run and evaluate budget verdict.
    pub fn record_tokens(
        &mut self,
        prompt_tokens: usize,
        completion_tokens: usize,
    ) -> GuardrailVerdict {
        self.total_input_tokens += prompt_tokens;
        self.total_output_tokens += completion_tokens;

        let delta_cost = self.calculate_cost(prompt_tokens, completion_tokens);
        self.current_run_cost += delta_cost;
        self.accumulated_day_cost += delta_cost;

        self.evaluate()
    }

    /// Action configured in budget settings
    fn action(&self) -> BudgetAction {
        match self.budget.on_limit.as_str() {
            "downgrade" => BudgetAction::Downgrade,
            "stop" => BudgetAction::Stop,
            _ => BudgetAction::Pause,
        }
    }

    /// Evaluate current cost against budget limits without adding tokens.
    pub fn evaluate(&self) -> GuardrailVerdict {
        if !self.budget.enabled {
            return GuardrailVerdict::Ok;
        }

        // Check hard limits first: per-run or daily
        if self.budget.per_run_limit > 0.0 && self.current_run_cost >= self.budget.per_run_limit {
            return GuardrailVerdict::LimitExceeded {
                action: self.action(),
                reason: format!(
                    "Run cost {:.4} exceeded per-run limit {:.4}",
                    self.current_run_cost, self.budget.per_run_limit
                ),
                current_run_cost: self.current_run_cost,
                daily_cost: self.accumulated_day_cost,
                downgrade_model: self.budget.downgrade_model.clone(),
            };
        }

        if self.budget.daily_limit > 0.0 && self.accumulated_day_cost >= self.budget.daily_limit {
            return GuardrailVerdict::LimitExceeded {
                action: self.action(),
                reason: format!(
                    "Daily cost {:.4} exceeded daily limit {:.4}",
                    self.accumulated_day_cost, self.budget.daily_limit
                ),
                current_run_cost: self.current_run_cost,
                daily_cost: self.accumulated_day_cost,
                downgrade_model: self.budget.downgrade_model.clone(),
            };
        }

        // Check warning threshold
        let warn_pct = (self.budget.warn_threshold_pct as f64) / 100.0;
        let mut warn_triggered = false;
        let mut warn_reason = String::new();

        if self.budget.per_run_limit > 0.0
            && self.current_run_cost >= self.budget.per_run_limit * warn_pct
        {
            warn_triggered = true;
            warn_reason = format!(
                "Run cost {:.4} reached {}% of run limit {:.4}",
                self.current_run_cost, self.budget.warn_threshold_pct, self.budget.per_run_limit
            );
        }

        if self.budget.daily_limit > 0.0
            && self.accumulated_day_cost >= self.budget.daily_limit * warn_pct
        {
            warn_triggered = true;
            warn_reason = format!(
                "Daily cost {:.4} reached {}% of daily limit {:.4}",
                self.accumulated_day_cost, self.budget.warn_threshold_pct, self.budget.daily_limit
            );
        }

        if warn_triggered {
            GuardrailVerdict::Warn {
                current_run_cost: self.current_run_cost,
                daily_cost: self.accumulated_day_cost,
                downgrade_model: self.budget.downgrade_model.clone(),
                message: warn_reason,
            }
        } else {
            GuardrailVerdict::Ok
        }
    }

    /// Reset run cost for a new iteration/run while preserving daily cost.
    pub fn reset_run(&mut self) {
        self.current_run_cost = 0.0;
        self.warn_emitted = false;
        self.downgrade_applied = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_guardrail_returns_ok() {
        let budget = BudgetSettings {
            enabled: false,
            per_run_limit: 1.0,
            ..BudgetSettings::default()
        };
        let mut guardrail = CostGuardrail::new(budget, 10.0, 10.0);
        let verdict = guardrail.record_tokens(10_000_000, 10_000_000);
        assert_eq!(verdict, GuardrailVerdict::Ok);
    }

    #[test]
    fn cost_calculation_uses_rates_correctly() {
        let budget = BudgetSettings {
            enabled: true,
            ..BudgetSettings::default()
        };
        let guardrail = CostGuardrail::new(budget, 3.0, 15.0);
        // 1M prompt ($3) + 1M completion ($15) = $18
        let cost = guardrail.calculate_cost(1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 1e-6);
    }

    #[test]
    fn warning_threshold_triggers_warn_verdict() {
        let budget = BudgetSettings {
            enabled: true,
            per_run_limit: 10.0,
            warn_threshold_pct: 80,
            on_limit: "downgrade".to_string(),
            downgrade_model: Some("gpt-4o-mini".to_string()),
            ..BudgetSettings::default()
        };
        let mut guardrail = CostGuardrail::new(budget, 10.0, 0.0);

        // 700k tokens = $7.0 (below 80% = $8.0)
        let verdict1 = guardrail.record_tokens(700_000, 0);
        assert_eq!(verdict1, GuardrailVerdict::Ok);

        // Another 150k tokens = +$1.50 -> total $8.50 (>= $8.0, < $10.0)
        let verdict2 = guardrail.record_tokens(150_000, 0);
        match verdict2 {
            GuardrailVerdict::Warn {
                downgrade_model,
                current_run_cost,
                ..
            } => {
                assert_eq!(downgrade_model, Some("gpt-4o-mini".to_string()));
                assert!((current_run_cost - 8.5).abs() < 1e-6);
            }
            other => panic!("expected Warn verdict, got {:?}", other),
        }
    }

    #[test]
    fn run_budget_exceeded_triggers_limit_exceeded() {
        let budget = BudgetSettings {
            enabled: true,
            per_run_limit: 5.0,
            on_limit: "pause".to_string(),
            ..BudgetSettings::default()
        };
        let mut guardrail = CostGuardrail::new(budget, 10.0, 0.0);
        // 600k tokens = $6.0 > $5.0 per_run_limit
        let verdict = guardrail.record_tokens(600_000, 0);
        match verdict {
            GuardrailVerdict::LimitExceeded {
                action,
                reason,
                current_run_cost,
                ..
            } => {
                assert_eq!(action, BudgetAction::Pause);
                assert!(reason.contains("Run cost"));
                assert!((current_run_cost - 6.0).abs() < 1e-6);
            }
            other => panic!("expected LimitExceeded verdict, got {:?}", other),
        }
    }

    #[test]
    fn daily_budget_exceeded_triggers_limit_exceeded() {
        let budget = BudgetSettings {
            enabled: true,
            daily_limit: 20.0,
            on_limit: "stop".to_string(),
            ..BudgetSettings::default()
        };
        let mut guardrail = CostGuardrail::new(budget, 10.0, 0.0);
        // Run 1: 1M tokens ($10)
        let _ = guardrail.record_tokens(1_000_000, 0);
        guardrail.reset_run();
        assert_eq!(guardrail.current_run_cost, 0.0);
        assert_eq!(guardrail.accumulated_day_cost, 10.0);

        // Run 2: 1.2M tokens ($12) -> daily cost $22 > $20
        let verdict = guardrail.record_tokens(1_200_000, 0);
        match verdict {
            GuardrailVerdict::LimitExceeded {
                action,
                daily_cost,
                reason,
                ..
            } => {
                assert_eq!(action, BudgetAction::Stop);
                assert!(reason.contains("Daily cost"));
                assert!((daily_cost - 22.0).abs() < 1e-6);
            }
            other => panic!("expected LimitExceeded verdict, got {:?}", other),
        }
    }
}
