//! Conservative order-statistic bounds under an explicit IID model.
use sift_protocol::{
    BenchmarkMedianInterval as Median, BenchmarkMedianMethod, BenchmarkMedianUncertainty as Report,
    BenchmarkNumericInterval as Interval, BenchmarkOutcome, BenchmarkReport,
};
use std::cmp::Ordering;
const ASSUMPTIONS:&str="Conditional IID model: observations within each population must be independent and identically distributed. Serial correlation, cache drift, interleaving or changing load can invalidate coverage; these assumptions are not verified. Each median interval has at least 97.5% model coverage; Bonferroni gives at least 95% joint coverage, without independence between populations. Ties are conservative; observed zero width does not prove zero variability. These user-supplied measurements do not establish data/environment equivalence, causality, statistical regression or future equality; repeated comparisons have no multiplicity correction.";

fn population(report: &BenchmarkReport) -> Result<Vec<u64>, &'static str> {
    if !matches!(report.version, 1 | 2)
        || !report.completed
        || report.warmups > 100
        || !(1..=10_000).contains(&report.requested_iterations)
        || report.samples.len() != (report.warmups + report.requested_iterations) as usize
    {
        return Err(
            "A complete bounded population with exactly the requested observations is required",
        );
    }
    if report.samples.iter().enumerate().any(|(index, s)| {
        s.ordinal != index as u32
            || s.warmup != (index < report.warmups as usize)
            || s.outcome != BenchmarkOutcome::Success
            || s.client_elapsed_ns
                .is_some_and(|client| client != s.elapsed_ns)
    }) {
        return Err("Population ordinals, phases, outcomes or client-elapsed aliases are inconsistent; no samples were discarded to obtain bounds");
    }
    let mut values = report
        .samples
        .iter()
        .filter(|s| !s.warmup)
        .map(|s| s.elapsed_ns)
        .collect::<Vec<_>>();
    values.sort_unstable();
    Ok(values)
}

// Mode-normalized binomial weights avoid starting from underflowed 2^-n.
fn lower_rank(n: usize) -> Option<usize> {
    if !(7..=10_000).contains(&n) {
        return None;
    }
    let mode = n / 2;
    let mut weights = vec![0.0; n + 1];
    weights[mode] = 1.0;
    for j in (1..=mode).rev() {
        weights[j - 1] = weights[j] * (j as f64) / ((n - j + 1) as f64);
    }
    for j in mode..n {
        weights[j + 1] = weights[j] * ((n - j) as f64) / ((j + 1) as f64);
    }
    let total: f64 = weights.iter().sum();
    let mut tail = 0.0;
    let mut rank = None;
    for k in 1..=n.div_ceil(2) {
        tail += weights[k - 1] / total;
        // Conservatively pad floating error rather than accepting a boundary
        // whose computed probability rounds just below the requested cutoff.
        if 2.0 * tail + 1e-12 <= 0.025 {
            rank = Some(k);
        } else {
            break;
        }
    }
    rank
}
fn interval(values: &[u64]) -> Option<Median> {
    let n = values.len();
    let k = lower_rank(n)?;
    Some(Median {
        samples: n,
        lower_rank: k,
        upper_rank: n - k + 1,
        lower_ns: values[k - 1],
        upper_ns: values[n - k],
    })
}

fn scaled_cmp(left: u128, exponent: i32, right: u128) -> Ordering {
    if left == 0 {
        return 0u128.cmp(&right);
    }
    if right == 0 {
        return Ordering::Greater;
    }
    let left_bits = 128 - left.leading_zeros() as i32;
    let right_bits = 128 - right.leading_zeros() as i32;
    match (left_bits + exponent).cmp(&right_bits) {
        Ordering::Equal if exponent >= 0 => (left << exponent as u32).cmp(&right),
        Ordering::Equal => left.cmp(&(right << (-exponent) as u32)),
        order => order,
    }
}
// Compare an IEEE-754 number to the exact rational using at most 117-bit
// products. Equal effective bit lengths make the alignment shifts fit u128.
fn rational_cmp(value: f64, numerator: i128, denominator: u64) -> Ordering {
    if numerator == 0 {
        return value.partial_cmp(&0.0).unwrap();
    }
    let bits = value.abs().to_bits();
    let encoded = ((bits >> 52) & 0x7ff) as i32;
    let (mantissa, exponent) = if encoded == 0 {
        (bits & ((1u64 << 52) - 1), -1074)
    } else {
        (
            (bits & ((1u64 << 52) - 1)) | (1u64 << 52),
            encoded - 1023 - 52,
        )
    };
    let order = scaled_cmp(
        u128::from(mantissa) * u128::from(denominator),
        exponent,
        numerator.unsigned_abs(),
    );
    if numerator < 0 {
        order.reverse()
    } else {
        order
    }
}
fn bound(numerator: i128, denominator: u64, upper: bool) -> Option<f64> {
    if denominator == 0 {
        return None;
    }
    let mut value = numerator as f64 / denominator as f64;
    // Rounded numerator/denominator division is near the exact rational.
    // Bound work explicitly and withhold instead of publishing a wrong side.
    for _ in 0..8 {
        let order = rational_cmp(value, numerator, denominator);
        if order == Ordering::Equal
            || (upper && order == Ordering::Greater)
            || (!upper && order == Ordering::Less)
        {
            return value.is_finite().then_some(value);
        }
        let bits = value.to_bits();
        value = f64::from_bits(if (value >= 0.0) == upper {
            bits + 1
        } else {
            bits - 1
        });
    }
    None
}
fn difference(a: &Median, b: &Median) -> Option<Interval> {
    Some(Interval {
        lower: bound(i128::from(b.lower_ns) - i128::from(a.upper_ns), 1, false)?,
        upper: bound(i128::from(b.upper_ns) - i128::from(a.lower_ns), 1, true)?,
    })
}
fn relative(a: &Median, b: &Median) -> Option<Interval> {
    Some(Interval {
        lower: bound(
            (i128::from(b.lower_ns) - i128::from(a.upper_ns)) * 100,
            a.upper_ns,
            false,
        )?,
        upper: bound(
            (i128::from(b.upper_ns) - i128::from(a.lower_ns)) * 100,
            a.lower_ns,
            true,
        )?,
    })
}

pub(super) fn analyze(base: &BenchmarkReport, current: &BenchmarkReport, matching: bool) -> Report {
    let mut report = Report {
        method: BenchmarkMedianMethod::BinomialOrderStatisticsBonferroniV1,
        joint_confidence_level: 0.95,
        population_confidence_level: 0.975,
        baseline: None,
        candidate: None,
        delta_ns: None,
        delta_percent: None,
        unavailable_reason: None,
        relative_unavailable_reason: None,
        assumptions: ASSUMPTIONS.into(),
    };
    let result = (|| -> Result<(), &'static str> {
        if !matching {
            return Err(
                "Incompatible configurations or incomplete populations: model intervals withheld",
            );
        }
        let a = population(base)?;
        let b = population(current)?;
        report.baseline = interval(&a);
        report.candidate = interval(&b);
        let (Some(a), Some(b)) = (&report.baseline, &report.candidate) else {
            return Err("At least seven measured observations per complete population are needed for finite model intervals");
        };
        report.delta_ns = difference(a, b);
        if report.delta_ns.is_none() {
            return Err("Derived numerical bounds are unavailable");
        }
        if a.lower_ns == 0 {
            report.relative_unavailable_reason = Some(
                "Baseline model interval includes zero; finite relative bounds are unavailable"
                    .into(),
            );
        } else {
            report.delta_percent = relative(a, b);
            if report.delta_percent.is_none() {
                report.relative_unavailable_reason =
                    Some("Derived relative numerical bounds are unavailable".into());
            }
        }
        Ok(())
    })();
    report.unavailable_reason = result.err().map(str::to_owned);
    report
}
fn median_twice(report: &BenchmarkReport) -> Option<u128> {
    if report.samples.len() > 10_100 {
        return None;
    }
    let mut values = report
        .samples
        .iter()
        .filter(|s| !s.warmup && s.outcome == BenchmarkOutcome::Success)
        .map(|s| s.elapsed_ns)
        .collect::<Vec<_>>();
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        None
    } else {
        Some(u128::from(values[(n - 1) / 2]) + u128::from(values[n / 2]))
    }
}
pub(super) fn point_delta(a: &BenchmarkReport, b: &BenchmarkReport) -> Option<(f64, Option<f64>)> {
    let a = median_twice(a)?;
    let b = median_twice(b)?;
    let difference = b as i128 - a as i128;
    Some((
        difference as f64 / 2.0,
        (a > 0).then(|| difference as f64 / a as f64 * 100.0),
    ))
}

fn decimal_millis(units: i128) -> String {
    let magnitude = units.unsigned_abs();
    format!(
        "{}{}.{:03}",
        if units < 0 { "-" } else { "+" },
        magnitude / 1000,
        magnitude % 1000
    )
}
// Exact binary scaling before decimal rounding, so formatting cannot narrow
// a model bound. All derived percentages fit comfortably within i128.
fn percent_display(value: f64, upper: bool) -> String {
    let bits = value.abs().to_bits();
    let encoded = ((bits >> 52) & 0x7ff) as i32;
    let (mantissa, exponent) = if encoded == 0 {
        (bits & ((1u64 << 52) - 1), -1074)
    } else {
        (
            (bits & ((1u64 << 52) - 1)) | (1u64 << 52),
            encoded - 1023 - 52,
        )
    };
    let product = u128::from(mantissa) * 1000;
    let (whole, remainder) = if exponent >= 0 {
        (product << exponent as u32, false)
    } else if exponent <= -128 {
        (0, product != 0)
    } else {
        let shift = (-exponent) as u32;
        (product >> shift, product & ((1u128 << shift) - 1) != 0)
    };
    let negative = value.is_sign_negative();
    let magnitude = whole + u128::from(remainder && (upper != negative));
    decimal_millis(if negative {
        -(magnitude as i128)
    } else {
        magnitude as i128
    })
}
/// Bounded plain text shared by Vim and human-readable consumers. Endpoint
/// display rounds outward to 0.001 ms and 0.001%, independently of point labels.
pub fn describe(report: &Report) -> String {
    let bounds = match &report.delta_ns {
        Some(bounds) => {
            // Absolute endpoints are integer differences before float conversion;
            // representable outward float endpoints are also integral here.
            let lower = decimal_millis((bounds.lower as i128).div_euclid(1000));
            let upper = decimal_millis((bounds.upper as i128 + 999).div_euclid(1000));
            let relative = report.delta_percent.as_ref().map_or_else(
                || {
                    format!(
                        "; {}",
                        report
                            .relative_unavailable_reason
                            .as_deref()
                            .unwrap_or("relative bounds unavailable")
                    )
                },
                |p| {
                    format!(
                        "; relative [{}, {}]%",
                        percent_display(p.lower, false),
                        percent_display(p.upper, true)
                    )
                },
            );
            format!("IID model joint 95% median delta [{lower}, {upper}] ms{relative}. Display endpoints rounded outward.")
        }
        None => format!(
            "Median model interval unavailable: {}.",
            report
                .unavailable_reason
                .as_deref()
                .unwrap_or("insufficient evidence")
        ),
    };
    format!("{bounds} {}", report.assumptions)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(values: &[u64]) -> BenchmarkReport {
        serde_json::from_value(serde_json::json!({"version":2,"run_id":uuid::Uuid::new_v4(),"engine":"sqlite","sql":"SELECT 1","captured_at":"2026-11-05T00:00:00Z","warmups":0,"requested_iterations":values.len(),"query_timeout_ms":1000,"total_budget_ms":60000,"delay_ms":0,"parameter_count":0,"completed":true,"warnings":[],"median_ns":999999999.0,"samples":values.iter().enumerate().map(|(i,n)|serde_json::json!({"ordinal":i,"warmup":false,"outcome":"success","elapsed_ns":n,"client_elapsed_ns":n})).collect::<Vec<_>>()})).unwrap()
    }
    #[test]
    fn ranks_match_exact_integer_binomial_oracle_and_large_population_fixtures() {
        for n in 1usize..=64 {
            let mut coefficient = 1u128;
            let mut tail = 0u128;
            let mut expected = None;
            for k in 1..=n.div_ceil(2) {
                tail += coefficient;
                if 80 * tail <= 1u128 << n {
                    expected = Some(k);
                } else {
                    break;
                }
                coefficient = coefficient * (n - k + 1) as u128 / k as u128;
            }
            assert_eq!(lower_rank(n), expected, "n={n}");
        }
        // Independently evaluated using Python arbitrary-precision integer
        // binomial coefficients and 80 * tail <= 2**n, no float CDF.
        for (n, k) in [(100, 39), (1000, 465), (10000, 4888)] {
            assert_eq!(lower_rank(n), Some(k));
        }
        assert_eq!(lower_rank(10001), None);
    }
    #[test]
    fn tied_zero_and_maximum_clocks_retain_conservative_finite_bounds() {
        let comparison =
            crate::performance::compare_benchmarks(&report(&[10; 10]), &report(&[20; 10]));
        assert_eq!(comparison.delta_ns, Some(10.0));
        let model = comparison.median_uncertainty.unwrap();
        let a = model.baseline.unwrap();
        assert_eq!(
            (a.lower_rank, a.upper_rank, a.lower_ns, a.upper_ns),
            (2, 9, 10, 10)
        );
        assert_eq!(
            (
                model.delta_ns.as_ref().unwrap().lower,
                model.delta_ns.unwrap().upper
            ),
            (10.0, 10.0)
        );
        assert!(model.assumptions.contains("zero width does not prove"));
        let comparison =
            crate::performance::compare_benchmarks(&report(&[0; 7]), &report(&[20; 7]));
        assert_eq!(comparison.delta_ns, Some(20.0));
        assert_eq!(comparison.delta_percent, None);
        let model = comparison.median_uncertainty.unwrap();
        assert!(model.delta_ns.is_some() && model.delta_percent.is_none());
        assert!(model.relative_unavailable_reason.unwrap().contains("zero"));
        let comparison = crate::performance::compare_benchmarks(
            &report(&[u64::MAX - 1; 7]),
            &report(&[u64::MAX; 7]),
        );
        assert_eq!(
            comparison.delta_ns,
            Some(1.0),
            "subtract integer medians before float conversion"
        );
        let model = comparison.median_uncertainty.unwrap();
        let delta = model.delta_ns.unwrap();
        assert_eq!((delta.lower, delta.upper), (1.0, 1.0));
        let relative = model.delta_percent.unwrap();
        assert!(
            relative.lower > 0.0 && relative.lower <= relative.upper && relative.upper.is_finite()
        );
        let model = analyze(&report(&[0; 7]), &report(&[u64::MAX; 7]), true);
        let bounds = model.delta_ns.unwrap();
        assert_eq!(bounds.lower, 18_446_744_073_709_549_568.0);
        assert_eq!(bounds.upper, 18_446_744_073_709_551_616.0);
    }
    #[test]
    fn exact_rational_rounding_uses_both_sides_of_non_binary_fractions() {
        assert_eq!(
            bound(1, 10, false),
            Some(f64::from_bits(0.1f64.to_bits() - 1))
        );
        assert_eq!(bound(1, 10, true), Some(0.1));
        assert_eq!(bound(-1, 10, false), Some(-0.1));
        assert_eq!(
            bound(-1, 10, true),
            Some(-f64::from_bits(0.1f64.to_bits() - 1))
        );
        assert_eq!(bound(0, 10, true), Some(0.0));
        assert_eq!(bound(1, 0, true), None);
    }
    #[test]
    fn display_rounds_outward_and_explains_a_zero_relative_denominator() {
        let model = analyze(&report(&[10; 7]), &report(&[20; 7]), true);
        assert!(
            describe(&model).contains("[+0.000, +0.001] ms"),
            "sub-microsecond model differences must not look like a zero-width interval"
        );
        let zero = analyze(&report(&[0; 7]), &report(&[20; 7]), true);
        assert!(describe(&zero).contains("Baseline model interval includes zero"));
        assert_eq!(percent_display(0.1, false), "+0.100");
        assert_eq!(percent_display(0.1, true), "+0.101");
        assert_eq!(percent_display(-0.1, false), "-0.101");
        assert_eq!(percent_display(-0.1, true), "-0.100");
        assert_eq!(percent_display(f64::MIN_POSITIVE, false), "+0.000");
        assert_eq!(percent_display(f64::MIN_POSITIVE, true), "+0.001");
    }
    #[test]
    fn malformed_failed_tiny_and_incompatible_populations_cannot_gain_intervals() {
        let base = report(&[10; 8]);
        for mutation in 0..7 {
            let mut current = base.clone();
            match mutation {
                0 => current.samples[0].outcome = BenchmarkOutcome::Failed,
                1 => current.samples[0].ordinal = 2,
                2 => current.samples[0].warmup = true,
                3 => current.samples[0].client_elapsed_ns = Some(11),
                4 => {
                    current.samples.pop();
                }
                5 => current.completed = false,
                _ => current.query_timeout_ms += 1,
            }
            let model = crate::performance::compare_benchmarks(&base, &current)
                .median_uncertainty
                .unwrap();
            assert!(
                model.delta_ns.is_none() && model.unavailable_reason.is_some(),
                "case {mutation}"
            );
        }
        let model = analyze(&report(&[10; 6]), &report(&[20; 6]), true);
        assert!(model.unavailable_reason.unwrap().contains("seven"));
        let mut warm = base.clone();
        warm.warmups = 1;
        let mut first = warm.samples[0].clone();
        first.warmup = true;
        first.elapsed_ns = u64::MAX;
        first.client_elapsed_ns = Some(u64::MAX);
        warm.samples.insert(0, first);
        for (index, s) in warm.samples.iter_mut().enumerate() {
            s.ordinal = index as u32;
        }
        let model = analyze(&warm, &warm, true);
        assert_eq!(model.baseline.unwrap().upper_ns, 10, "warmups are excluded");
        warm.samples[0].outcome = BenchmarkOutcome::TimedOut;
        assert!(
            analyze(&warm, &warm, true).unavailable_reason.is_some(),
            "failed warmups cannot produce a complete IID population"
        );
    }
}
