#!/usr/bin/env bash
# Start coderd in Docker, bootstrap an owner and a default chat model, and run the ignored smoke tests.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
ref="$(cut -d' ' -f1 < "$root/spec/coder-ref.txt")"
if [[ $# -ge 1 ]]; then
  image="$1"
elif [[ "$ref" == v* ]]; then
  image="ghcr.io/coder/coder:$ref"
else
  image="ghcr.io/coder/coder-preview:latest"
fi
name="coder-sdk-smoke-$$"
port=37123
url="http://127.0.0.1:$port"

cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# The image's entrypoint is already `/opt/coder server`; passing a "server"
# argument here would append a second, unrecognized "server" subcommand.
docker run -d --name "$name" -p "$port:3000" \
  -e CODER_HTTP_ADDRESS=0.0.0.0:3000 -e CODER_ACCESS_URL="$url" -e CODER_TELEMETRY_ENABLE=false \
  "$image" >/dev/null

for _ in $(seq 1 120); do
  curl -fsS "$url/healthz" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "$url/healthz" >/dev/null

email="smoke@example.com"
password="SmokeTest-Only-$$-Password"
curl -fsS -X POST "$url/api/v2/users/first" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"username\":\"smoke\",\"password\":\"$password\",\"trial\":false}" >/dev/null
token="$(curl -fsS -X POST "$url/api/v2/users/login" -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"password\":\"$password\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session_token"])')"
org="$(curl -fsS "$url/api/v2/users/me/organizations" -H "Coder-Session-Token: $token" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"

provider="$(curl -fsS -X POST "$url/api/v2/ai/providers" -H "Coder-Session-Token: $token" -H 'Content-Type: application/json' \
  -d '{"type":"openai-compat","name":"smoke","enabled":true,"base_url":"http://127.0.0.1:9/v1","api_keys":["smoke-key-not-real"]}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
curl -fsS -X POST "$url/api/v2/organizations/$org/chats/models" -H "Coder-Session-Token: $token" -H 'Content-Type: application/json' \
  -d "{\"ai_provider_id\":\"$provider\",\"model\":\"gpt-4o-mini\",\"context_limit\":4096,\"is_default\":true}" >/dev/null

(cd "$root" && CODER_URL="$url" CODER_SESSION_TOKEN="$token" CODER_SMOKE_ORG="$org" \
  cargo test -p coder-sdk --test smoke -- --ignored --test-threads=1)
