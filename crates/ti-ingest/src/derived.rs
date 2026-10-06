//! Derived event rules for Lume TI.
//!
//! Enforces:
//! - `@starts` state transitions (e.g. into `started`)
//! - Boolean rising edges (transitions <= 0 to > 0)
//! - Notification states and raise counts

use std::collections::BTreeMap;
use ti_contracts::{DerivedKind, DerivedRule};

use crate::normalize::matches_glob;

#[derive(Debug, Clone, Default)]
pub struct DerivedTracker {
    rules: Vec<DerivedRule>,
    prev_states: BTreeMap<(String, String), String>,
    prev_numerics: BTreeMap<(String, String), f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DerivedEvent {
    Transition { output: String },
    RisingEdge { output: String },
    NotificationRaise { output: String },
}

impl DerivedTracker {
    pub fn new(rules: &[DerivedRule]) -> Self {
        Self {
            rules: rules.to_vec(),
            prev_states: BTreeMap::new(),
            prev_numerics: BTreeMap::new(),
        }
    }

    /// Process a string/state value. Returns any triggered derived events.
    pub fn on_state_value(&mut self, context: &str, path: &str, state: &str) -> Vec<DerivedEvent> {
        let mut events = Vec::new();
        let key = (context.to_string(), path.to_string());
        let prev = self.prev_states.insert(key, state.to_string());

        for rule in &self.rules {
            if matches_glob(&rule.path, path) {
                match rule.kind {
                    DerivedKind::Transition => {
                        if let Some(target) = &rule.state {
                            if state == target && prev.as_deref() != Some(target) {
                                let output = substitute_glob_output(&rule.path, path, &rule.output);
                                events.push(DerivedEvent::Transition { output });
                            }
                        }
                    }
                    DerivedKind::Notification => {
                        let is_raised = !["nominal", "normal"].contains(&state);
                        let was_raised = prev
                            .as_ref()
                            .is_some_and(|p| !["nominal", "normal"].contains(&p.as_str()));
                        if is_raised && !was_raised {
                            let output = format!("{path}@count");
                            events.push(DerivedEvent::NotificationRaise { output });
                        }
                    }
                    _ => {}
                }
            }
        }

        events
    }

    /// Process a numeric/boolean value for rising edges.
    pub fn on_numeric_value(&mut self, context: &str, path: &str, value: f64) -> Vec<DerivedEvent> {
        let mut events = Vec::new();
        let key = (context.to_string(), path.to_string());
        let prev = self.prev_numerics.insert(key, value);

        for rule in &self.rules {
            if matches_glob(&rule.path, path) && rule.kind == DerivedKind::RisingEdge {
                let was_low = prev.is_none_or(|p| p <= 0.0);
                let is_high = value > 0.0;
                if was_low && is_high {
                    let output = substitute_glob_output(&rule.path, path, &rule.output);
                    events.push(DerivedEvent::RisingEdge { output });
                }
            }
        }

        events
    }
}

/// Substitute wildcards in output path if pattern contained `*`.
/// e.g. pattern `propulsion.*.state` and path `propulsion.port.state`
/// with output `propulsion.*.state@starts` -> `propulsion.port.state@starts`.
fn substitute_glob_output(pattern: &str, path: &str, output: &str) -> String {
    if !pattern.contains('*') || !output.contains('*') {
        return output.to_string();
    }

    let p_prefix = pattern.split('*').next().unwrap_or("");
    let p_suffix = pattern.split('*').nth(1).unwrap_or("");

    if path.starts_with(p_prefix) && path.ends_with(p_suffix) {
        let star_content = &path[p_prefix.len()..path.len() - p_suffix.len()];
        output.replace('*', star_content)
    } else {
        output.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_starts() {
        let rule = DerivedRule {
            path: "propulsion.*.state".into(),
            output: "propulsion.*.state@starts".into(),
            kind: DerivedKind::Transition,
            state: Some("started".into()),
        };
        let mut tracker = DerivedTracker::new(&[rule]);
        let ctx = "vessels.self";

        // Stopped -> started => event
        let e1 = tracker.on_state_value(ctx, "propulsion.port.state", "stopped");
        assert!(e1.is_empty());
        let e2 = tracker.on_state_value(ctx, "propulsion.port.state", "started");
        assert_eq!(e2.len(), 1);
        assert_eq!(
            e2[0],
            DerivedEvent::Transition {
                output: "propulsion.port.state@starts".into()
            }
        );

        // Already started -> no duplicate transition
        let e3 = tracker.on_state_value(ctx, "propulsion.port.state", "started");
        assert!(e3.is_empty());
    }

    #[test]
    fn test_rising_edge() {
        let rule = DerivedRule {
            path: "bilge.pump.active".into(),
            output: "bilge.pump.active@edges".into(),
            kind: DerivedKind::RisingEdge,
            state: None,
        };
        let mut tracker = DerivedTracker::new(&[rule]);
        let ctx = "vessels.self";

        assert!(tracker
            .on_numeric_value(ctx, "bilge.pump.active", 0.0)
            .is_empty());
        let e = tracker.on_numeric_value(ctx, "bilge.pump.active", 1.0);
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0],
            DerivedEvent::RisingEdge {
                output: "bilge.pump.active@edges".into()
            }
        );
    }
}
