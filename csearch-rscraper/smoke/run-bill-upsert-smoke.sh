#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SMOKE_DIR="$ROOT_DIR/backend/scraper/smoke"
COMPOSE_FILE="$SMOKE_DIR/docker-compose.yml"
SAMPLE_BILL="$ROOT_DIR/backend/scraper/congress/data/119/bills/hr/hr1/data.json"
UPDATED_TITLE="FEHB Protection Act of 2025 [rscraper smoke]"
PROJECT_NAME="rscraper-smoke-$(date +%s)"
RUNTIME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/rscraper-runtime.XXXXXX")"

export COMPOSE_PROJECT_NAME="$PROJECT_NAME"
export SMOKE_RUNTIME_DIR="$RUNTIME_DIR"

cleanup() {
  docker compose -f "$COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$RUNTIME_DIR"
}
trap cleanup EXIT

if [[ ! -f "$SAMPLE_BILL" ]]; then
  echo "sample bill fixture not found: $SAMPLE_BILL" >&2
  exit 1
fi

python3 - "$ROOT_DIR" "$RUNTIME_DIR" "$SAMPLE_BILL" <<'PY'
import json
import shutil
import sys
from pathlib import Path

root = Path(sys.argv[1])
runtime = Path(sys.argv[2])
sample_bill = Path(sys.argv[3])

congress_root = runtime / "congress"
(runtime / "data").mkdir(parents=True, exist_ok=True)
(congress_root / "data").mkdir(parents=True, exist_ok=True)

run_py = congress_root / "run.py"
run_py.write_text(
    "#!/usr/bin/env python3\n"
    "import json, sys\n"
    "print(json.dumps({\"args\": sys.argv[1:], \"smoke\": True}))\n",
    encoding="utf-8",
)
run_py.chmod(0o755)

for rel_name in ["__init__.py"]:
    src = root / "backend" / "scraper" / "congress" / rel_name
    if src.exists():
        shutil.copy2(src, congress_root / rel_name)

bill_tables = ["s", "hr", "hconres", "hjres", "hres", "sconres", "sjres", "sres"]
for congress in range(93, 120):
    for table in bill_tables:
        (congress_root / "data" / str(congress) / "bills" / table).mkdir(parents=True, exist_ok=True)

dest = congress_root / "data" / "119" / "bills" / "hr" / "hr1" / "data.json"
dest.parent.mkdir(parents=True, exist_ok=True)
shutil.copy2(sample_bill, dest)
for path in [runtime, *runtime.rglob("*")]:
    if path.is_dir():
        path.chmod(0o777)
    elif path.suffix == ".json":
        path.chmod(0o666)
    else:
        path.chmod(0o777)
PY

echo "Building smoke-test images..."
docker compose -f "$COMPOSE_FILE" build postgres rscraper >/dev/null

echo "Starting fresh Postgres..."
docker compose -f "$COMPOSE_FILE" up -d postgres >/dev/null

for _ in $(seq 1 60); do
  if docker compose -f "$COMPOSE_FILE" exec -T postgres pg_isready -U postgres -d csearch >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! docker compose -f "$COMPOSE_FILE" exec -T postgres pg_isready -U postgres -d csearch >/dev/null 2>&1; then
  echo "postgres did not become ready" >&2
  exit 1
fi

echo "Running initial bill ingest..."
docker compose -f "$COMPOSE_FILE" run --rm rscraper >/dev/null

bill_count="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select count(*) from bills where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"
action_count="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select count(*) from bill_actions where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"
first_title="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select coalesce(shorttitle, '') from bills where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"

if [[ "$bill_count" != "1" ]]; then
  echo "expected one bill row after initial ingest, got: $bill_count" >&2
  exit 1
fi

if [[ "$action_count" -le 0 ]]; then
  echo "expected bill_actions rows after initial ingest, got: $action_count" >&2
  exit 1
fi

python3 - "$RUNTIME_DIR/congress/data/119/bills/hr/hr1/data.json" "$UPDATED_TITLE" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
updated_title = sys.argv[2]
data = json.loads(path.read_text(encoding="utf-8"))
data["short_title"] = updated_title
if "summary" in data and isinstance(data["summary"], dict):
    text = data["summary"].get("text") or ""
    data["summary"]["text"] = text + "\n\nSmoke test update marker."
path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
PY

echo "Running second ingest to verify upsert behavior..."
docker compose -f "$COMPOSE_FILE" run --rm rscraper >/dev/null

second_bill_count="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select count(*) from bills where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"
second_title="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select coalesce(shorttitle, '') from bills where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"
second_summary_has_marker="$(
  docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U postgres -d csearch -At \
    -c "select position('Smoke test update marker.' in coalesce(summary_text, '')) > 0 from bills where billtype = 'hr' and billnumber = 1 and congress = 119;"
)"

if [[ "$second_bill_count" != "1" ]]; then
  echo "expected one bill row after upsert run, got: $second_bill_count" >&2
  exit 1
fi

if [[ "$second_title" != "$UPDATED_TITLE" ]]; then
  echo "expected updated title after upsert, got: $second_title" >&2
  exit 1
fi

if [[ "$second_summary_has_marker" != "t" ]]; then
  echo "expected updated summary text marker after upsert" >&2
  exit 1
fi

cat <<EOF
Smoke test passed.
Initial title: $first_title
Updated title: $second_title
Initial bill_actions rows: $action_count
EOF
