#!/usr/bin/env bash
# Call one tool of the fleet admin MCP (/fleet/mcp, docs/fleet.md) with the
# FLEET_ADMIN_TOKEN of .env, and print its answer.
#
#   scripts/fleet.sh tools                                    the tool names
#   scripts/fleet.sh fleet_list                               every device, pending ones too
#   scripts/fleet.sh fleet_approve '{"mac":"aa:bb:cc:dd:ee:ff","name":"desk"}'
#   scripts/fleet.sh fleet_lock '{"device":"desk","locked":true}'
#
# It calls https://$BOTSTACK_DOMAIN; BOTSTACK_URL overrides that, e.g.
# BOTSTACK_URL=http://127.0.0.1:8080 for a local bridge. Exits 1 when the
# tool answers an error.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
env_file="$root/.env"
# shellcheck source-path=SCRIPTDIR source=dotenv.sh
. "$root/scripts/dotenv.sh"

usage() { echo "usage: $0 tools | <tool> ['<json arguments>']" >&2; exit 2; }
[[ $# -ge 1 && $# -le 2 ]] || usage
[[ -f "$env_file" ]] || { echo "no .env: run scripts/setup.sh first" >&2; exit 1; }
token=$(env_get FLEET_ADMIN_TOKEN)
[[ -n "$token" ]] || { echo "FLEET_ADMIN_TOKEN is empty in .env: run scripts/setup.sh" >&2; exit 1; }
url=${BOTSTACK_URL:-}
if [[ -z "$url" ]]; then
  domain=$(env_get BOTSTACK_DOMAIN)
  [[ -n "$domain" ]] || { echo "BOTSTACK_DOMAIN is empty in .env (or set BOTSTACK_URL)" >&2; exit 1; }
  url="https://$domain"
fi
url="${url%/}/fleet/mcp"

if [[ "$1" == tools ]]; then
  [[ $# -eq 1 ]] || usage
  method=tools/list tool="" arguments="{}"
else
  [[ "$1" =~ ^[a-z_]{1,64}$ ]] || usage
  method=tools/call tool=$1 arguments=${2:-"{}"}
fi
body=$(python3 -c '
import json, sys
method, tool, arguments = sys.argv[1:]
try:
    arguments = json.loads(arguments)
except ValueError as error:
    sys.exit(f"the arguments are not JSON: {error}")
if not isinstance(arguments, dict):
    sys.exit("the arguments must be a JSON object")
params = {"name": tool, "arguments": arguments} if method == "tools/call" else {}
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
' "$method" "$tool" "$arguments")

# The header comes from a file descriptor, so the token stays out of the command line.
response=$(curl -sS --max-time 30 -w '\n%{http_code}' -H @<(printf 'Authorization: Bearer %s\n' "$token") \
  -H 'Content-Type: application/json' --data-binary @- "$url" <<<"$body")
unset token
status=${response##*$'\n'}
response=${response%$'\n'*}

python3 -c '
import json, sys
method, status, text = sys.argv[1:]
try:
    answer = json.loads(text)
except ValueError:
    sys.exit(f"HTTP {status}: {text[:200]}")
if status != "200":
    error = answer.get("error") if isinstance(answer, dict) else None
    sys.exit(f"HTTP {status}: {error or text[:200]}")
if "error" in answer:
    sys.exit(answer["error"].get("message", "error"))
result = answer["result"]
if method == "tools/list":
    for tool in result["tools"]:
        print(tool["name"])
    sys.exit(0)
print(result["content"][0]["text"])
sys.exit(1 if result.get("isError") else 0)
' "$method" "$status" "$response"
