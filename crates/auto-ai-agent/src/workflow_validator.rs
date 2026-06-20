//! Workflow output validators — quality gates for step outputs.
//!
//! Ported-of-spirit from auto-forge's `relay/flow.rs` StepValidator. A
//! validator checks a step's textual output and passes/fails it. Validators
//! compose with [`Validator::All`] / [`Validator::Any`].
//!
//! Used by the workflow engine: when a step finishes, its `validators` run;
//! if any fail (and the step has an `on_fail` retry), the engine loops back.

use serde::{Deserialize, Serialize};

/// A validator that checks a step's output. Serializable for `.at` config.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Validator {
    /// Output must contain the `pattern` string.
    OutputContains { pattern: String },
    /// Output must NOT contain the `pattern` string.
    OutputNotContains { pattern: String },
    /// Output must be at least `min` characters (guards against empty output).
    OutputMinLength { min: usize },
    /// All of the inner validators must pass.
    All { validators: Vec<Validator> },
    /// At least one of the inner validators must pass.
    Any { validators: Vec<Validator> },
}

impl Validator {
    /// Run this validator against `output`. Returns `Ok(())` on pass,
    /// `Err(message)` on fail (message explains what failed).
    pub fn check(&self, output: &str) -> Result<(), String> {
        match self {
            Validator::OutputContains { pattern } => {
                if output.contains(pattern.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "output must contain '{pattern}' (it doesn't)"
                    ))
                }
            }
            Validator::OutputNotContains { pattern } => {
                if !output.contains(pattern.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "output must NOT contain '{pattern}' (it does)"
                    ))
                }
            }
            Validator::OutputMinLength { min } => {
                if output.len() >= *min {
                    Ok(())
                } else {
                    Err(format!(
                        "output is {} chars, must be at least {min}",
                        output.len()
                    ))
                }
            }
            Validator::All { validators } => {
                for v in validators {
                    v.check(output)?;
                }
                Ok(())
            }
            Validator::Any { validators } => {
                let errors: Vec<String> = validators
                    .iter()
                    .filter_map(|v| v.check(output).err())
                    .collect();
                if errors.len() < validators.len() {
                    Ok(()) // at least one passed
                } else {
                    Err(format!("none of {} validators passed", validators.len()))
                }
            }
        }
    }
}

/// Run a list of validators against `output`. Passes only if ALL pass.
/// Returns the first failure message, or Ok if all pass.
pub fn check_all(validators: &[Validator], output: &str) -> Result<(), String> {
    for v in validators {
        v.check(output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_contains_pass() {
        assert!(Validator::OutputContains { pattern: "OK".into() }
            .check("status: OK")
            .is_ok());
    }

    #[test]
    fn output_contains_fail() {
        assert!(Validator::OutputContains { pattern: "OK".into() }
            .check("status: pending")
            .is_err());
    }

    #[test]
    fn output_not_contains_pass() {
        assert!(Validator::OutputNotContains { pattern: "FAIL".into() }
            .check("all good")
            .is_ok());
    }

    #[test]
    fn output_not_contains_fail() {
        assert!(Validator::OutputNotContains { pattern: "FAIL".into() }
            .check("tests FAIL")
            .is_err());
    }

    #[test]
    fn output_min_length_pass() {
        assert!(Validator::OutputMinLength { min: 5 }
            .check("hello world")
            .is_ok());
    }

    #[test]
    fn output_min_length_fail() {
        assert!(Validator::OutputMinLength { min: 100 }
            .check("short")
            .is_err());
    }

    #[test]
    fn all_passes_when_all_inner_pass() {
        let v = Validator::All {
            validators: vec![
                Validator::OutputContains { pattern: "A".into() },
                Validator::OutputContains { pattern: "B".into() },
            ],
        };
        assert!(v.check("A and B").is_ok());
    }

    #[test]
    fn all_fails_when_one_inner_fails() {
        let v = Validator::All {
            validators: vec![
                Validator::OutputContains { pattern: "A".into() },
                Validator::OutputContains { pattern: "Z".into() },
            ],
        };
        assert!(v.check("A and B").is_err());
    }

    #[test]
    fn any_passes_when_one_inner_passes() {
        let v = Validator::Any {
            validators: vec![
                Validator::OutputContains { pattern: "Z".into() },
                Validator::OutputContains { pattern: "A".into() },
            ],
        };
        assert!(v.check("A and B").is_ok());
    }

    #[test]
    fn any_fails_when_all_inner_fail() {
        let v = Validator::Any {
            validators: vec![
                Validator::OutputContains { pattern: "X".into() },
                Validator::OutputContains { pattern: "Z".into() },
            ],
        };
        assert!(v.check("A and B").is_err());
    }

    #[test]
    fn check_all_helper() {
        let vs = vec![Validator::OutputContains { pattern: "a".into() }];
        assert!(check_all(&vs, "abc").is_ok());
        assert!(check_all(&vs, "xyz").is_err());
    }

    #[test]
    fn empty_validators_list_passes() {
        assert!(check_all(&[], "anything").is_ok());
    }
}
