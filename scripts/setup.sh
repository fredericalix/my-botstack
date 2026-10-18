#!/usr/bin/env bash
# Prepare .env for docker compose and the two files the firmware embeds.
# Run it again at any time: it fills only what is missing and never replaces
# a secret or a file that is already there.
#
#   scripts/setup.sh
#
# It asks for the domain and the xAI API key (typed without echo). Without a
# terminal, give them in the environment: BOTSTACK_DOMAIN=... XAI_API_KEY=...
# It never prints a secret, only what it did.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
env_file="$root/.env"
# shellcheck source-path=SCRIPTDIR source=dotenv.sh
. "$root/scripts/dotenv.sh"

command -v openssl >/dev/null || { echo "openssl is needed to generate the keys" >&2; exit 1; }

if [[ ! -f "$env_file" ]]; then
  (umask 077 && cp "$root/.env.example" "$env_file")
  echo ".env created from .env.example"
fi
chmod 600 "$env_file"

# The domain: the server's public name, for Caddy's certificate and the robots.
domain=$(env_get BOTSTACK_DOMAIN)
if [[ -z "$domain" || "$domain" == bot.example.com ]]; then
  domain=${BOTSTACK_DOMAIN:-}
  if [[ -z "$domain" && -t 0 ]]; then
    read -rp "Domain of this server (its DNS points here, ports 80 and 443 open): " domain
  fi
  domain=$(printf '%s' "$domain" | tr '[:upper:]' '[:lower:]')
  if [[ ! "$domain" =~ ^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$ ]]; then
    echo "a domain name such as bot.example.com is needed (or BOTSTACK_DOMAIN=...)" >&2
    exit 1
  fi
  env_set BOTSTACK_DOMAIN "$domain"
  echo "BOTSTACK_DOMAIN set to $domain"
fi
# PUBLIC_URL stays empty: the bridge derives it from BOTSTACK_DOMAIN, so a
# later edit of the domain alone is enough. An older .env may hold the
# example's or a copy of the domain: clear it.
public=$(env_get PUBLIC_URL)
if [[ "$public" == https://bot.example.com || "$public" == "https://$domain" ]]; then
  env_set PUBLIC_URL ""
  echo "PUBLIC_URL cleared (the bridge derives it from BOTSTACK_DOMAIN)"
elif [[ -n "$public" ]]; then
  echo "PUBLIC_URL is set to $public; it overrides https://$domain"
fi

# The xAI API key: asked once, never echoed.
if [[ -z "$(env_get XAI_API_KEY)" ]]; then
  key=${XAI_API_KEY:-}
  if [[ -z "$key" && -t 0 ]]; then
    read -rsp "xAI API key (https://console.x.ai; not shown): " key
    echo
  fi
  if [[ -z "$key" ]]; then
    echo "XAI_API_KEY left empty: the robots cannot talk until it is set; run scripts/setup.sh again" >&2
  elif [[ "$key" =~ [[:space:]\"\'\$\#\\] ]]; then
    echo "XAI_API_KEY refused: it contains spaces, quotes, \$, # or \\" >&2
    exit 1
  else
    env_set XAI_API_KEY "$key"
    echo "XAI_API_KEY set (${#key} characters)"
  fi
  unset key
fi

# The robot's bootstrap key: the one already in firmware/robot-token if there
# is one (robots flashed with it keep working), else a new one.
token_file="$root/firmware/robot-token"
robot=$(env_get ROBOT_TOKEN)
if [[ -z "$robot" ]]; then
  if [[ -s "$token_file" ]]; then
    robot=$(tr -d '[:space:]' <"$token_file")
    env_set ROBOT_TOKEN "$robot"
    echo "ROBOT_TOKEN taken from firmware/robot-token (${#robot} characters)"
  else
    robot=$(openssl rand -hex 32)
    env_set ROBOT_TOKEN "$robot"
    echo "ROBOT_TOKEN generated"
  fi
fi
for name in GROK_BRIDGE_TOKEN FLEET_ADMIN_TOKEN; do
  if [[ -z "$(env_get "$name")" ]]; then
    env_set "$name" "$(openssl rand -hex 32)"
    echo "$name generated"
  fi
done

# The two files the firmware embeds (gitignored, mode 600).
if [[ ! -e "$token_file" ]]; then
  (umask 077 && printf '%s\n' "$robot" >"$token_file")
  echo "firmware/robot-token written"
elif [[ "$(tr -d '[:space:]' <"$token_file")" != "$robot" ]]; then
  echo "firmware/robot-token differs from ROBOT_TOKEN in .env; left as is (robots flashed with it are refused until both match)" >&2
fi
url_file="$root/firmware/bridge-url"
if [[ ! -e "$url_file" ]]; then
  (umask 077 && printf 'wss://%s/botstack/v1\n' "$domain" >"$url_file")
  echo "firmware/bridge-url written (wss://$domain/botstack/v1)"
elif [[ "$(tr -d '[:space:]' <"$url_file")" != "wss://$domain/botstack/v1" ]]; then
  echo "firmware/bridge-url does not name wss://$domain/botstack/v1; left as is" >&2
fi
unset robot

echo "ready: docker compose up -d --build"
