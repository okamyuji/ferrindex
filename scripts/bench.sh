#!/bin/bash
# 五つの実リポジトリで索引の作成時間・最大RSS・find の中央値・出力量を測る。
# 使い方: scripts/bench.sh [リポジトリ名...]   （省略時は全部）
# 数値は実行時に測って標準出力へ出すだけで、ファイルへは書かない。
set -euo pipefail

BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/ferrindex"
BENCH="${FERRINDEX_BENCH_DIR:-$HOME/.cache/ferrindex-bench}"
# 相対パスのままだと、リポジトリへ cd した後の解決先がずれる
mkdir -p "$BENCH"
BENCH="$(cd "$BENCH" && pwd)"
# 利用者の本物の索引キャッシュに触れない
export XDG_CACHE_HOME="$BENCH/cache"

# 名前 | GitHub の repo | tag | find と rg で比べる三つの名前
REPOS=(
  "linux|torvalds/linux|v6.12|schedule printk mutex_lock"
  "kubernetes|kubernetes/kubernetes|v1.31.0|NewController Validate Reconcile"
  "vscode|microsoft/vscode|1.95.0|activate Disposable dispose"
  "django|django/django|5.1|QuerySet get_queryset save"
  "rails|rails/rails|v8.0.0|validates find_by render"
)

for tool in hyperfine rg python3 /usr/bin/time; do
  command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
done
[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }

want=("$@")
selected() {
  [ ${#want[@]} -eq 0 ] && return 0
  local w
  for w in "${want[@]}"; do [ "$w" = "$1" ] && return 0; done
  return 1
}

# 目標: find の中央値(ms)。5,000ファイル以下は50、linux規模は1000、それ以外は500。
find_target_ms() {
  if [ "$1" = linux ]; then echo 1000
  elif [ "$2" -le 5000 ]; then echo 50
  else echo 500
  fi
}
BUILD_LIMIT_S=60
RSS_LIMIT_MB=2048

build_rows=()
bytes_rows=()

for spec in "${REPOS[@]}"; do
  IFS='|' read -r name repo tag names <<<"$spec"
  selected "$name" || continue
  dir="$BENCH/$name"
  if [ ! -d "$dir" ]; then
    mkdir -p "$BENCH"
    git clone -q --depth 1 --branch "$tag" "https://github.com/$repo" "$dir"
  fi
  cd "$dir"

  timing="$(mktemp)"
  /usr/bin/time -l "$BIN" update --rebuild >/dev/null 2>"$timing"
  build_s="$(awk '/ real /{print $1}' "$timing")"
  rss_mb="$(awk '/maximum resident set size/{printf "%d", $1/1048576}' "$timing")"
  rm -f "$timing"

  files="$("$BIN" update | awk '{print $2}')"
  index_bytes="$("$BIN" update --stats | awk '/^total/{print $2}')"

  first="${names%% *}"
  json="$(mktemp)"
  hyperfine -N --warmup 3 --runs 30 --export-json "$json" "\"$BIN\" find $first" >/dev/null
  median_ms="$(python3 -I -c 'import json,statistics,sys; r=json.load(open(sys.argv[1]))["results"][0]; t=r["times"] if "times" in r else [x["time_wall_clock"]["value"] for x in r["measurements"]]; print(round(statistics.median(t)*1000,1))' "$json")"
  rm -f "$json"

  target_ms="$(find_target_ms "$name" "$files")"
  verdict=pass
  python3 -I -c 'import sys; m,t,b,bl,r,rl=map(float,sys.argv[1:]); sys.exit(0 if m<=t and b<=bl and r<=rl else 1)' \
    "$median_ms" "$target_ms" "$build_s" "$BUILD_LIMIT_S" "$rss_mb" "$RSS_LIMIT_MB" || verdict=FAIL
  build_rows+=("| $name | $files | ${build_s} s | ${rss_mb} MB | $((index_bytes / 1024)) KB | ${median_ms} ms | <= ${target_ms} ms | $verdict |")

  for n in $names; do
    f_bytes="$("$BIN" find "$n" | wc -c | tr -d ' ')"
    r_bytes="$(rg -n "$n" . | wc -c | tr -d ' ')"
    bytes_rows+=("| $name | $n | $f_bytes | $r_bytes |")
  done
done

echo "| repo | files | build | max RSS | index size | find median | target | result |"
echo "|---|---|---|---|---|---|---|---|"
printf '%s\n' "${build_rows[@]}"
echo
echo "| repo | name | ferrindex find (bytes) | rg -n (bytes) |"
echo "|---|---|---|---|"
printf '%s\n' "${bytes_rows[@]}"
