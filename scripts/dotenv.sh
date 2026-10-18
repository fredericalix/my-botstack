# shellcheck shell=bash disable=SC2154
# Read and write one KEY=value line of a .env file, with bash builtins only, so
# a secret never appears in a command line. Sourced by setup.sh and fleet.sh;
# they set env_file first.

# The value of the last uncommented KEY= line, quotes removed; empty if none.
env_get() {
  local line value=""
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == "$1="* ]] && value=${line#"$1="}
  done <"$env_file"
  value=${value%$'\r'}
  if [[ ${#value} -ge 2 && ( "$value" == \"*\" || "$value" == \'*\' ) ]]; then
    value=${value:1:${#value}-2}
  fi
  printf '%s' "$value"
}

# Replace every KEY= line with KEY=value, or append it. The file stays mode 600.
env_set() {
  local key=$1 value=$2 line found=0 tmp="$env_file.tmp"
  (umask 077 && : >"$tmp")
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == "$key="* ]]; then
      [[ $found == 1 ]] || printf '%s=%s\n' "$key" "$value"
      found=1
    else
      printf '%s\n' "$line"
    fi
  done <"$env_file" >"$tmp"
  [[ $found == 1 ]] || printf '%s=%s\n' "$key" "$value" >>"$tmp"
  mv "$tmp" "$env_file"
}
