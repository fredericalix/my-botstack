# Changelog

## Unreleased

### Breaking

- `GROK_BOT_WEBHOOK_URL` must be `https://`. An install whose webhook URL is
  `http://` does not start until the URL is changed, or
  `GROK_BOT_WEBHOOK_ALLOW_HTTP` is set to `1`, `true`, or `yes` for a local
  bridge only. The webhook key would otherwise travel in clear.
