#!/usr/bin/env bash
# Runs the API's integration tests against a throwaway PostgreSQL database.
#
# Usage:  PGHOST=/tmp/pgsock PGPORT=5544 PGUSER=pgrun ./run_integration.sh
# It needs a running PostgreSQL 14+ where PGUSER may create databases and roles.
# It builds a fresh database, applies every migration in ../migrations in order,
# creates a non-superuser login (so row-level security is really tested), runs
# the tests, and drops the database again.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DB="plat_api_test_$$"
HOST="${PGHOST:-localhost}"
PORT="${PGPORT:-5432}"
ADMIN_USER="${PGUSER:-postgres}"

psql_admin() { psql -h "$HOST" -p "$PORT" -U "$ADMIN_USER" -v ON_ERROR_STOP=1 -q "$@"; }

cleanup() { psql_admin -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || true; }
trap cleanup EXIT

psql_admin -d postgres -c "CREATE DATABASE $DB"
for f in "$HERE"/../migrations/*.sql; do
  echo "applying $(basename "$f")"
  psql_admin -d "$DB" -f "$f"
done

# A login that is NOT a superuser and does NOT own the tables.
psql_admin -d postgres -c "DO \$\$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='app_test') THEN
    CREATE ROLE app_test LOGIN PASSWORD 'app_test' IN ROLE platform_app;
  END IF; END \$\$;"

# A socket directory (starts with /) goes in ?host=; a normal host name goes in the URL.
if [[ "$HOST" == /* ]]; then
  AUTH_HOST="localhost"; SOCK="&host=$HOST"
else
  AUTH_HOST="$HOST"; SOCK=""
fi
export ADMIN_DATABASE_URL="postgres://$ADMIN_USER@$AUTH_HOST:$PORT/$DB?sslmode=disable$SOCK"
export DATABASE_URL="postgres://app_test:app_test@$AUTH_HOST:$PORT/$DB?sslmode=disable$SOCK"
cd "$HERE"
cargo test --test money_path --test radius_link --test members --test admin_api --test refunds -- --test-threads=4 "$@"
