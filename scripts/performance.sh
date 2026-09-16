#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

usage() {
  cat <<'EOF'
Usage:
  scripts/performance.sh run [benchmark-filter]
  scripts/performance.sh baseline <name> [benchmark-filter]
  scripts/performance.sh compare <name> [benchmark-filter]
  scripts/performance.sh profile [desktop-arguments...]

Environment:
  SIFT_PERF_SAMPLES       Criterion sample count (default: 10)
  SIFT_PERF_WARMUP        warmup seconds (default: 1)
  SIFT_PERF_MEASUREMENT   measurement seconds (default: 2)
  SIFT_PERF_DIR           artifact root (default: target/performance)
  SIFT_PERF_DISPLAY_HZ    physical display refresh rate recorded as metadata
  SIFT_PERF_DRY_RUN       write metadata and print command without running it
EOF
}

action="${1:-run}"
shift || true

case "$action" in
  run)
    baseline_name=""
    benchmark_filter="${1:-}"
    ;;
  baseline | compare)
    baseline_name="${1:-}"
    if [[ ! "$baseline_name" =~ ^[A-Za-z0-9._-]+$ ]]; then
      echo "baseline name must contain only letters, digits, dot, underscore, or dash" >&2
      exit 2
    fi
    shift
    benchmark_filter="${1:-}"
    ;;
  profile)
    if ! command -v samply >/dev/null 2>&1; then
      echo "samply is required: https://github.com/mstange/samply" >&2
      exit 2
    fi
    cargo build -p sift-desktop --profile release-dev
    exec samply record target/release-dev/sift-desktop "$@"
    ;;
  -h | --help | help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
label="${baseline_name:-all}"
artifact_root="${SIFT_PERF_DIR:-target/performance}"
artifact_dir="$artifact_root/${stamp}-${action}-${label}"
mkdir -p "$artifact_root"
mkdir "$artifact_dir"

if [[ -z "$(git status --porcelain --untracked-files=normal)" ]]; then
  dirty=false
else
  dirty=true
fi

{
  echo "timestamp_utc=$stamp"
  echo "action=$action"
  echo "benchmark_filter=${benchmark_filter:-all}"
  echo "baseline_name=${baseline_name:-none}"
  echo "git_commit=$(git rev-parse HEAD)"
  echo "git_dirty=$dirty"
  echo "display_hz=${SIFT_PERF_DISPLAY_HZ:-unknown}"
  echo "system=$(uname -a)"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  if command -v lscpu >/dev/null 2>&1; then
    lscpu | sed -n 's/^Model name:[[:space:]]*/cpu=/p'
  elif command -v sysctl >/dev/null 2>&1; then
    echo "cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
  fi
} >"$artifact_dir/metadata.txt"

criterion_args=(
  --sample-size "${SIFT_PERF_SAMPLES:-10}"
  --warm-up-time "${SIFT_PERF_WARMUP:-1}"
  --measurement-time "${SIFT_PERF_MEASUREMENT:-2}"
)
case "$action" in
  baseline) criterion_args+=(--save-baseline "$baseline_name") ;;
  compare) criterion_args+=(--baseline "$baseline_name") ;;
esac

command=(
  cargo bench
  -p sift-workspace-ui
  --features benchmark
  --profile release-dev
  --bench frame_budget
)
if [[ -n "$benchmark_filter" ]]; then
  command+=("$benchmark_filter")
fi
command+=(-- "${criterion_args[@]}")

printf '%q ' "${command[@]}" >"$artifact_dir/command.txt"
printf '\n' >>"$artifact_dir/command.txt"

echo "performance artifacts: $artifact_dir"
if [[ "${SIFT_PERF_DRY_RUN:-0}" == "1" ]]; then
  cat "$artifact_dir/command.txt"
  exit 0
fi
"${command[@]}" 2>&1 | tee "$artifact_dir/benchmark.log"
