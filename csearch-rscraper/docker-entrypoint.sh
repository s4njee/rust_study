#!/bin/sh
set -eu

runtime_root="${CONGRESSDIR:-/srv/csearch}"
postgres_host="${POSTGRESURI:-postgres}"
postgres_user="${DB_USER:-postgres}"
postgres_db="${DB_NAME:-csearch}"

mkdir -p "$runtime_root"
mkdir -p "$runtime_root/data" "$runtime_root/cache"

if [ ! -f "$runtime_root/congress/run.py" ]; then
    mkdir -p "$runtime_root/congress" 2>/dev/null || true
    if [ -w "$runtime_root/congress" ]; then
        cp -R /opt/csearch/congress/. "$runtime_root/congress"
    fi
fi

mkdir -p "$runtime_root/congress/data"

export PYTHONPATH="$runtime_root${PYTHONPATH:+:$PYTHONPATH}"

until pg_isready -h "$postgres_host" -U "$postgres_user" -d "$postgres_db" >/dev/null 2>&1; do
    echo "waiting for postgres at $postgres_host..."
    sleep 2
done

exec "$@"
