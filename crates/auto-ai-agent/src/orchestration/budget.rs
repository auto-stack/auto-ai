//! Token Budgeting — tracks and enforces per-step and per-run token spend.
//!
//! Makes cost a first-class constraint, not a surprise bill.
//! (Plan 008 Phase 1 — moved from musk relay/budget.rs, no changes needed.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Token budget with limit, warning threshold, and enforcement strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub limit: u64,
    pub warning_at: u64,
    pub strategy: BudgetStrategy,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self::new(20_000_000)
    }
}

impl TokenBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            warning_at: (limit as f64 * 0.7) as u64,
            strategy: BudgetStrategy::HardStop,
        }
    }

    pub fn with_strategy(limit: u64, strategy: BudgetStrategy) -> Self {
        Self {
            limit,
            warning_at: (limit as f64 * 0.7) as u64,
            strategy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetStrategy {
    /// Halt step and request human decision.
    HardStop,
    /// Switch to a cheaper model for the remainder.
    EscalateModel,
    /// Aggressively compress context.
    SummarizeContext,
    /// Skip non-critical work.
    SkipOptional,
}

/// Action the pipeline should take after a budget check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetAction {
    /// Spend within limits — proceed.
    None,
    /// Approaching a limit — warn but continue.
    Warning { remaining: u64 },
    /// Hard limit reached — halt.
    HardStop,
}

/// Tracks token usage across a run, per step and cumulatively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetTracker {
    pub run_budget: TokenBudget,
    pub step_budgets: HashMap<String, TokenBudget>,
    pub cumulative: u64,
    pub per_step: HashMap<String, u64>,
}

impl BudgetTracker {
    pub fn new(run_budget: TokenBudget) -> Self {
        Self {
            run_budget,
            step_budgets: HashMap::new(),
            cumulative: 0,
            per_step: HashMap::new(),
        }
    }

    /// Record token usage from an API response.
    pub fn record(&mut self, step: &str, input: u64, output: u64) {
        let total = input + output;
        self.cumulative += total;
        *self.per_step.entry(step.to_string()).or_insert(0) += total;
    }

    /// Check whether current spend triggers any budget action.
    pub fn check(&self, step: &str) -> BudgetAction {
        let step_used = self.per_step.get(step).copied().unwrap_or(0);

        // Step budget first (tighter constraint).
        if let Some(step_budget) = self.step_budgets.get(step) {
            if step_used >= step_budget.limit {
                return BudgetAction::HardStop;
            }
            if step_used >= step_budget.warning_at {
                return BudgetAction::Warning {
                    remaining: step_budget.limit - step_used,
                };
            }
        }

        // Then the run-wide budget.
        if self.cumulative >= self.run_budget.limit {
            return BudgetAction::HardStop;
        }
        if self.cumulative >= self.run_budget.warning_at {
            return BudgetAction::Warning {
                remaining: self.run_budget.limit - self.cumulative,
            };
        }

        BudgetAction::None
    }

    /// Set a per-step budget.
    pub fn set_step_budget(&mut self, step: &str, budget: TokenBudget) {
        self.step_budgets.insert(step.to_string(), budget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_then_hardstop_at_run_budget() {
        let mut t = BudgetTracker::new(TokenBudget::new(1000));
        // 0.7 × 1000 = 700 warning threshold.
        t.record("code", 700, 0);
        assert_eq!(t.check("code"), BudgetAction::Warning { remaining: 300 });
        t.record("code", 350, 0);
        assert_eq!(t.check("code"), BudgetAction::HardStop);
    }

    #[test]
    fn step_budget_overrides() {
        let mut t = BudgetTracker::new(TokenBudget::new(100_000));
        t.set_step_budget("code", TokenBudget::new(500));
        t.record("code", 500, 0);
        assert_eq!(t.check("code"), BudgetAction::HardStop);
        // Run budget still fine.
        assert!(t.cumulative < t.run_budget.limit);
    }

    #[test]
    fn no_action_when_under_budget() {
        let mut t = BudgetTracker::new(TokenBudget::new(10_000));
        t.record("design", 100, 50);
        assert_eq!(t.check("design"), BudgetAction::None);
    }
}
