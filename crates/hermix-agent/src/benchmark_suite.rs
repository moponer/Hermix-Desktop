// P6 - Benchmark suite.
//
// Four sub-primitive scoreboards for the runtime regressions.
// Each one is a small data type + a couple of policy functions;
// the actual workload drivers live in `tests/` directories.
//
// P6-T1 AdversarialHarness - harness-level attack replay
// P6-T2 SkillHealthScoreboard - skill health trend over time
// P6-T3 RouterAccuracyBenchmark - routing Top-1 / Top-3 accuracy
// P6-T4 EffectivenessBenchmark - end-to-end task completion
//
// Pure data + pure functions. No I/O unless explicitly required.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// P6-T1 - AdversarialHarness
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AdversarialOutcome {
    pub case_name: String,
    pub matched_expected: bool,
    pub observed: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdversarialReport {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f32,
}

impl AdversarialReport {
    pub fn aggregate(outcomes: &[AdversarialOutcome]) -> Self {
        let total = outcomes.len();
        let passed = outcomes.iter().filter(|o| o.matched_expected).count();
        let failed = total - passed;
        let pass_rate = if total == 0 {
            1.0
        } else {
            passed as f32 / total as f32
        };
        Self {
            total,
            passed,
            failed,
            pass_rate,
        }
    }

    pub fn is_passing(&self, threshold: f32) -> bool {
        self.pass_rate >= threshold
    }
}

// ---------------------------------------------------------------------------
// P6-T2 - SkillHealthScoreboard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SkillHealthSnapshot {
    pub name: String,
    pub score: f32, // 0.0 - 1.0
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillHealthReport {
    pub avg: f32,
    pub min: f32,
    pub max: f32,
    pub below_count: usize,
}

impl SkillHealthReport {
    pub fn aggregate(snapshots: &[SkillHealthSnapshot], threshold: f32) -> Self {
        if snapshots.is_empty() {
            return Self {
                avg: 1.0,
                min: 1.0,
                max: 1.0,
                below_count: 0,
            };
        }
        let mut sum = 0.0_f32;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut below = 0usize;
        for s in snapshots {
            sum += s.score;
            if s.score < min {
                min = s.score;
            }
            if s.score > max {
                max = s.score;
            }
            if s.score < threshold {
                below += 1;
            }
        }
        Self {
            avg: sum / snapshots.len() as f32,
            min,
            max,
            below_count: below,
        }
    }
}

// ---------------------------------------------------------------------------
// P6-T3 - RouterAccuracyBenchmark
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct RouterGuess {
    pub expected_top: String,
    pub observed_ranking: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouterAccuracyReport {
    pub cases: usize,
    pub top1_correct: usize,
    pub top3_correct: usize,
    pub mrr: f32,
}

impl RouterAccuracyReport {
    pub fn aggregate(guesses: &[RouterGuess]) -> Self {
        let cases = guesses.len();
        let mut top1 = 0;
        let mut top3 = 0;
        let mut mrr_sum = 0.0_f32;
        for g in guesses {
            if g.observed_ranking.first() == Some(&g.expected_top) {
                top1 += 1;
            }
            for (idx, name) in g.observed_ranking.iter().take(3).enumerate() {
                if *name == g.expected_top {
                    top3 += 1;
                    mrr_sum += 1.0 / (idx as f32 + 1.0);
                    break;
                }
            }
        }
        let mrr = if cases == 0 {
            0.0
        } else {
            mrr_sum / cases as f32
        };
        Self {
            cases,
            top1_correct: top1,
            top3_correct: top3,
            mrr,
        }
    }

    pub fn top1_rate(&self) -> f32 {
        if self.cases == 0 {
            0.0
        } else {
            self.top1_correct as f32 / self.cases as f32
        }
    }

    pub fn top3_rate(&self) -> f32 {
        if self.cases == 0 {
            0.0
        } else {
            self.top3_correct as f32 / self.cases as f32
        }
    }
}

// ---------------------------------------------------------------------------
// P6-T4 - EffectivenessBenchmark
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    Completed,
    PartiallyCompleted,
    Escalated,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectivenessSample {
    pub outcome: TaskOutcome,
    pub turns_used: u32,
    pub tools_invoked: u32,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectivenessReport {
    pub samples: usize,
    pub completed: usize,
    pub partial: usize,
    pub escalation_rate: f32,
    pub avg_turns: f32,
    pub avg_tools: f32,
}

impl EffectivenessReport {
    pub fn aggregate(samples: &[EffectivenessSample]) -> Self {
        if samples.is_empty() {
            return Self {
                samples: 0,
                completed: 0,
                partial: 0,
                escalation_rate: 0.0,
                avg_turns: 0.0,
                avg_tools: 0.0,
            };
        }
        let mut completed = 0;
        let mut partial = 0;
        let mut escalated = 0;
        let mut turns_sum = 0u64;
        let mut tools_sum = 0u64;
        for s in samples {
            match s.outcome {
                TaskOutcome::Completed => completed += 1,
                TaskOutcome::PartiallyCompleted => partial += 1,
                TaskOutcome::Escalated => escalated += 1,
                TaskOutcome::Abandoned => {}
            }
            turns_sum += s.turns_used as u64;
            tools_sum += s.tools_invoked as u64;
        }
        let n = samples.len() as f32;
        Self {
            samples: samples.len(),
            completed,
            partial,
            escalation_rate: escalated as f32 / n,
            avg_turns: turns_sum as f32 / n,
            avg_tools: tools_sum as f32 / n,
        }
    }
}

// ---------------------------------------------------------------------------
// Reusable query/scenario maps (P6 helpers)
// ---------------------------------------------------------------------------

/// Compute a flat (case_name -> measured_pass_rate) map. Useful for
/// the harness to dump CSV-shaped output without bundling whole
/// reports into a single struct.
pub fn flatten_pass_rates(reports: HashMap<String, AdversarialReport>) -> HashMap<String, f32> {
    reports
        .into_iter()
        .map(|(name, r)| (name, r.pass_rate))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // P6-T1
    #[test]
    fn adversarial_report_empty_passes() {
        let r = AdversarialReport::aggregate(&[]);
        assert_eq!(r.total, 0);
        assert_eq!(r.pass_rate, 1.0);
        assert!(r.is_passing(0.5));
    }

    #[test]
    fn adversarial_report_counts() {
        let outcomes = vec![
            AdversarialOutcome {
                case_name: "a".into(),
                matched_expected: true,
                observed: "ok".into(),
            },
            AdversarialOutcome {
                case_name: "b".into(),
                matched_expected: false,
                observed: "skip".into(),
            },
            AdversarialOutcome {
                case_name: "c".into(),
                matched_expected: true,
                observed: "ok".into(),
            },
        ];
        let r = AdversarialReport::aggregate(&outcomes);
        assert_eq!(r.passed, 2);
        assert_eq!(r.failed, 1);
        assert!((r.pass_rate - 2.0 / 3.0).abs() < 1e-6);
        assert!(!r.is_passing(0.9));
        assert!(r.is_passing(0.5));
    }

    // P6-T2
    #[test]
    fn skill_health_scoreboard_thresholds() {
        let snaps = vec![
            SkillHealthSnapshot { name: "s1".into(), score: 0.9 },
            SkillHealthSnapshot { name: "s2".into(), score: 0.6 },
            SkillHealthSnapshot { name: "s3".into(), score: 0.4 },
        ];
        let r = SkillHealthReport::aggregate(&snaps, 0.5);
        assert!((r.avg - (0.9 + 0.6 + 0.4) / 3.0).abs() < 1e-6);
        assert!(r.min.abs() < 1e-6 || (r.min - 0.4).abs() < 1e-6);
        assert!((r.max - 0.9).abs() < 1e-6);
        // below threshold: s3.
        assert_eq!(r.below_count, 1);
    }

    #[test]
    fn skill_health_scoreboard_empty_is_full_credit() {
        let r = SkillHealthReport::aggregate(&[], 0.5);
        assert_eq!(r.below_count, 0);
        assert_eq!(r.avg, 1.0);
    }

    // P6-T3
    #[test]
    fn router_accuracy_top1_vs_top3() {
        let guesses = vec![
            RouterGuess {
                expected_top: "x".into(),
                observed_ranking: vec!["x".into(), "y".into(), "z".into()],
            },
            RouterGuess {
                expected_top: "x".into(),
                observed_ranking: vec!["y".into(), "x".into(), "z".into()],
            },
            RouterGuess {
                expected_top: "x".into(),
                observed_ranking: vec!["a".into(), "b".into(), "x".into()],
            },
            RouterGuess {
                expected_top: "x".into(),
                observed_ranking: vec!["a".into(), "b".into(), "c".into()],
            },
        ];
        let r = RouterAccuracyReport::aggregate(&guesses);
        assert_eq!(r.cases, 4);
        assert_eq!(r.top1_correct, 1);
        assert_eq!(r.top3_correct, 3);
        // MRR: 1/1 + 1/2 + 1/3 + 0 = ~1.833/4 = ~0.458
        assert!((r.mrr - (1.0 + 0.5 + 1.0 / 3.0) / 4.0).abs() < 1e-5);
    }

    #[test]
    fn router_accuracy_zero_when_no_cases() {
        let r = RouterAccuracyReport::aggregate(&[]);
        assert_eq!(r.cases, 0);
        assert_eq!(r.top1_rate(), 0.0);
        assert_eq!(r.top3_rate(), 0.0);
    }

    // P6-T4
    #[test]
    fn effectiveness_report_aggregates() {
        let samples = vec![
            EffectivenessSample {
                outcome: TaskOutcome::Completed,
                turns_used: 4,
                tools_invoked: 6,
                tokens: 2_000,
            },
            EffectivenessSample {
                outcome: TaskOutcome::Escalated,
                turns_used: 8,
                tools_invoked: 12,
                tokens: 5_000,
            },
            EffectivenessSample {
                outcome: TaskOutcome::PartiallyCompleted,
                turns_used: 3,
                tools_invoked: 4,
                tokens: 1_500,
            },
        ];
        let r = EffectivenessReport::aggregate(&samples);
        assert_eq!(r.samples, 3);
        assert_eq!(r.completed, 1);
        assert_eq!(r.partial, 1);
        assert!((r.escalation_rate - 1.0 / 3.0).abs() < 1e-6);
        assert!((r.avg_turns - (4.0 + 8.0 + 3.0) / 3.0).abs() < 1e-6);
        assert!((r.avg_tools - (6.0 + 12.0 + 4.0) / 3.0).abs() < 1e-6);
    }

    #[test]
    fn effectiveness_report_empty() {
        let r = EffectivenessReport::aggregate(&[]);
        assert_eq!(r.samples, 0);
        assert_eq!(r.escalation_rate, 0.0);
        assert_eq!(r.avg_turns, 0.0);
    }

    // helpers
    #[test]
    fn flatten_pass_rates_preserves_map() {
        let mut m = HashMap::new();
        m.insert(
            "a".to_string(),
            AdversarialReport {
                total: 4,
                passed: 3,
                failed: 1,
                pass_rate: 0.75,
            },
        );
        m.insert(
            "b".to_string(),
            AdversarialReport {
                total: 2,
                passed: 2,
                failed: 0,
                pass_rate: 1.0,
            },
        );
        let f = flatten_pass_rates(m);
        assert_eq!(f.get("a"), Some(&0.75));
        assert_eq!(f.get("b"), Some(&1.0));
    }
}
