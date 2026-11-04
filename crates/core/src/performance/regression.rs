//! Descriptive threshold checks. No statistical or causal regression claim.
use serde::Serialize;
use sift_protocol::BenchmarkComparison;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RegressionLimits {
    pub milliseconds: Option<f64>,
    pub percent: Option<f64>,
}

impl RegressionLimits {
    pub fn validate(self) -> Result<(), &'static str> {
        if [self.milliseconds, self.percent]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("regression limits must be finite and nonnegative");
        }
        Ok(())
    }

    pub fn supplied(self) -> bool {
        self.milliseconds.is_some() || self.percent.is_some()
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    ExceedsLimit,
    WithinLimit,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub struct RegressionReport {
    pub limits: RegressionLimits,
    pub absolute: Option<Observation>,
    pub relative: Option<Observation>,
    pub uncertainty: &'static str,
}

pub fn evaluate(
    comparison: &BenchmarkComparison,
    limits: RegressionLimits,
) -> Result<RegressionReport, &'static str> {
    limits.validate()?;
    if !limits.supplied() {
        return Err("at least one regression limit is required");
    }
    let check = |limit: Option<f64>, observed: Option<f64>| {
        limit.map(|limit| match observed.filter(|value| value.is_finite()) {
            Some(value) if value > limit => Observation::ExceedsLimit,
            Some(_) => Observation::WithinLimit,
            None => Observation::Unavailable,
        })
    };
    Ok(RegressionReport {
        limits,
        absolute: check(limits.milliseconds, comparison.delta_ns.map(|ns| ns / 1_000_000.0)),
        relative: check(limits.percent, comparison.delta_percent),
        uncertainty: "Observed median thresholds only. Statistical significance, data/schema equivalence, cache and load control are unproven; within_limit is not proof of no regression. Read comparison warnings and sample outcome counts.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_reject_invalid_numbers() {
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(RegressionLimits {
                milliseconds: Some(value),
                percent: None
            }
            .validate()
            .is_err());
        }
        assert!(RegressionLimits {
            milliseconds: Some(0.0),
            percent: Some(5.0)
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn equality_missing_data_and_independent_limits_are_explicit() {
        let statistics = || sift_protocol::BenchmarkStatistics {
            successful: 10,
            failed: 0,
            timed_out: 0,
            cancelled: 0,
            median_ns: Some(1_000_000.0),
        };
        let mut comparison = BenchmarkComparison {
            timing: "server_side_client_elapsed_ns".into(),
            baseline: statistics(),
            candidate: statistics(),
            delta_ns: Some(1_000_000.0),
            delta_percent: Some(20.0),
            warnings: vec![],
        };
        let limits = RegressionLimits {
            milliseconds: Some(1.0),
            percent: Some(10.0),
        };
        let report = evaluate(&comparison, limits).unwrap();
        assert_eq!(report.absolute, Some(Observation::WithinLimit));
        assert_eq!(report.relative, Some(Observation::ExceedsLimit));
        comparison.delta_ns = None;
        comparison.delta_percent = None;
        let report = evaluate(&comparison, limits).unwrap();
        assert_eq!(report.absolute, Some(Observation::Unavailable));
        assert_eq!(report.relative, Some(Observation::Unavailable));
        assert!(evaluate(&comparison, RegressionLimits::default()).is_err());
    }
}
