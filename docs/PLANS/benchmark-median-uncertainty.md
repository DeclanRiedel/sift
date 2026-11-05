# Conditional median uncertainty for benchmark comparisons

Status: implemented and verified on Linux (ADR-150). Native Windows/macOS
acceptance remains deferred.

Extend the shared private comparison model with conservative finite median
intervals, preserving descriptive thresholds and existing compatibility gates.
Use non-interpolated binomial order statistics, not a normal approximation,
bootstrap population or instrumented Profile clock. Each population gets at
least 97.5% coverage under the IID model; Bonferroni gives at least 95% joint
coverage without assuming independence between populations. Difference/relative
median bounds follow from their rectangle. These are conditional model results,
not a controlled regression verdict or proof that sampling assumptions hold.

Require complete compatible reports with exactly the requested successful
measured observations, valid contiguous ordinals, complete successful warm-ups,
and consistent client-elapsed aliases. Bound populations at 10000 measured and
100 warm-up samples. Exclude warm-ups from interval input; never drop failures,
trim outliers or fabricate missing observations to obtain an interval. At least
seven measured observations per population are needed for finite 97.5% bounds.
Return explicit unavailable reasons when admission fails. A zero baseline lower
bound withholds relative bounds while retaining absolute bounds.

Choose the narrowest symmetric order-statistic interval whose two binomial
tails total no more than 0.025. Normalize binomial weights from the mode to
avoid large-n underflow, with a conservative numerical margin at the cutoff.
Preserve exact u64 order-statistic endpoints. Derive signed differences before
float conversion; round derived float bounds outward using exact integer/
binary-rational comparisons. No fixed-width subtraction, division by zero,
NaN/Infinity, Monte Carlo seed or unbounded iteration is admitted.

The model explicitly assumes independent, identically distributed measurements
within each population. Serial correlation, warm-cache drift, nonstationarity,
interleaving and environment/load changes can invalidate coverage. Ties are
allowed and conservative; an observed zero-width interval is not proof of
zero variance or future equality. Excluding zero is not a causal verdict, a
multiplicity-adjusted regression test or an equivalence result. Reports remain
user-supplied snapshots, without profile/data/cache attestation.

Compute the same result for CLI, AI/MCP measurement inspection and Vim live/saved
comparisons. Keep raw samples, SQL, plans, notes and source context out of the
AI/MCP projection. Cache desktop comparison presentation when measurements or
baselines change, so rendering does not repeatedly sort samples. Preserve
source ownership, existing audited reads and existing threshold exit behavior;
this work executes no database query.

Acceptance needs independent exact binomial-rank oracles, tiny/large/tied/zero/
u64-boundary cases, malformed/failed/incomplete population refusals, summary
forgery resistance, CLI/MCP privacy and UI refresh behavior. Run fmt, strict
Clippy and the full workspace suite. Controlled environments, population
independence evidence and native Windows/macOS acceptance remain open/deferred.

Method reference: NIST TN 2119, section 5.3, describes binomial order-statistic
confidence bounds for a population quantile under IID sampling, including the
conservative discrete case: https://doi.org/10.6028/NIST.TN.2119.


Coverage is conditional repeated-sampling coverage, not a posterior probability
for the observed interval. Completion/stopping selection, retries and timeout
censoring can violate the assumptions or change the target population; failure
risk and unconditional future latency are not estimated. Displayed bounds round
outward to 0.001 ms and 0.001%, preserving narrow intervals instead of rounding
them to a false zero-width display. Relative unavailable reasons remain visible.


## Acceptance evidence

Five focused numerical tests pass. Binomial ranks match an independent exact
integer oracle for n=1..64 and Python arbitrary-precision fixtures for n=100,
1000 and 10000. Tied/zero/max-u64 clocks retain finite conservative bounds and
small exact differences; positive/negative non-binary fractions round outward.
Tiny, incompatible, malformed, failed and incomplete populations withhold their
intervals, and warm-ups remain excluded. Display checks preserve sub-microsecond
ranges and explain unavailable zero-denominator relative bounds.

Existing real SQLite CLI/SDK acceptance runs eight ordinary measured attempts,
saves and compares them through the private MCP/CLI projection, verifies
aggregate model fields/assumptions, and retains SQL/plan/notes/source-context
privacy. Existing Vim tests recompute from samples rather than forged cached
medians and exercise cached presentation after baseline/report changes and stale
completion refusal. Rendering reuses comparison strings instead of sorting
measurements on every frame.

All 1,739 workspace tests pass (five existing ignored), with formatting and
strict workspace/all-target Clippy. This verifies the implementation and
admission behavior; it does not establish IID sampling for native workloads,
controlled cache/load, multiplicity-adjusted regression or result equivalence.
The broader product goal remains active.
