# Changelog

## Unreleased

### Breaking

- `GROK_BOT_WEBHOOK_URL` must be `https://`. An install whose webhook URL is
  `http://` does not start until the URL is changed, or
  `GROK_BOT_WEBHOOK_ALLOW_HTTP` is set to `1`, `true`, or `yes` for a local
  bridge only. The webhook key would otherwise travel in clear.

### Fixed

- The bridge's own "Grok Bot didn't answer." line (and "I can't reach Grok Bot") is no
  longer kept for the next connection when the robot drops while it is being said.
  Only Grok Bot's words go to `grok_pending.json`.
- A `stackchan_reply` whose text is empty once cleaned now answers the request: the
  robot no longer says that Grok Bot did not answer after a reply did arrive.
- `GROK_BOT_WEBHOOK_URL` without a host (`https://`) stops the bridge at start instead
  of failing at the first question.
