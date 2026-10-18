# Changelog

## Unreleased

### Breaking

- `GROK_BOT_WEBHOOK_URL` must be `https://` and must name a host. An install
  whose webhook URL is `http://` does not start until the URL is changed, or
  `GROK_BOT_WEBHOOK_ALLOW_HTTP` is set to `1`, `true`, or `yes` for a local
  bridge only. The webhook key would otherwise travel in clear.
- `stackchan_say` and `stackchan_photo` with no `device_id` no longer use the
  only approved device. They need an explicit `device_id` or an approved
  `GROK_DEVICE_ID`. A reply that names no device still uses the only approved
  device when `GROK_DEVICE_ID` is unset.
