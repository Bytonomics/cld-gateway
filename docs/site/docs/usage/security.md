# Security

## Localhost-only by default

The gateway binds only to `127.0.0.1` — it does not accept connections
from other machines on your network, and it does not listen on a wildcard
address. There is no supported configuration that exposes it beyond
localhost; if you need remote access, put a reverse proxy you control in
front of it and take responsibility for that proxy's own authentication.

## Outbound network policy

The gateway restricts which hosts it will make outbound requests to. By
default it can reach:

- its active backend's own hosts,
- localhost (for the login callback flow), and
- any additional hosts you explicitly list under `network.allowed_hosts`
  in your config.

One rule holds regardless of what you put in `allowed_hosts`: the gateway
will never contact Anthropic's own API hosts, even if you list them
explicitly and even if a redirect tries to send it there. This isn't
configurable — it exists so the gateway can't accidentally become a path
for your traffic to reach the API it's meant to be an alternative to.

## What's stored on disk, and what isn't

- **Stored:** backend credentials, in `~/.gateway/auth.json`. This file is
  the gateway's only copy of your login state — treat it like any other
  credential file (don't sync it to a shared machine, don't commit it to a
  repository).
- **Stored:** conversation state and exchange/transport logs, under
  `~/.gateway/`, as described in
  [Logs and state](../configuration/logs-and-state.md). Exchange log
  entries include request metadata needed for troubleshooting; treat log
  files with the same care you'd give any record of your conversations.
- **Not stored:** the gateway does not use your operating system's
  keychain or credential manager — everything it needs is in the one file
  above, on purpose, so there's exactly one place to look (and exactly one
  place to delete) if you want to revoke local access.

## If you need to revoke access

Deleting `~/.gateway/auth.json` removes the gateway's local credentials
immediately; you'll need to run `cld-gateway login` again before it can
serve requests. Revoking the underlying authorization on the backend's own
side (if it offers a connected-apps or sessions page) is a separate step
and the more thorough one if you suspect the credentials themselves were
compromised.
