#!/usr/bin/env bash
# Usage: migrate.sh <host> <owner-user> <database>   (PGPASSWORD and APP_DB_PASSWORD in the environment)
# Applies every migration in order, once, then creates the login the API uses.
set -euo pipefail
H="$1"; U="$2"; D="$3"
psql="psql -h $H -U $U -d $D -v ON_ERROR_STOP=1 -q"
$psql -c "CREATE TABLE IF NOT EXISTS schema_migrations (name text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())"
for f in /migrations/*.sql; do
  n=$(basename "$f")
  if [ "$($psql -tA -c "SELECT 1 FROM schema_migrations WHERE name='$n'")" = "1" ]; then echo "skip $n"; continue; fi
  echo "apply $n"
  $psql -1 -f "$f" -c "INSERT INTO schema_migrations (name) VALUES ('$n')"
done
# The API's login: a member of platform_app (not a superuser, not the table owner), so row-level security applies.
$psql -v pw="$APP_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE platform_login LOGIN PASSWORD %L IN ROLE platform_app', :'pw')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'platform_login') \gexec
SELECT format('ALTER ROLE platform_login PASSWORD %L', :'pw') \gexec
SQL
echo "done"
