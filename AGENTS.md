# Gateway Repo Notes

- When debugging Claude Code requests through the gateway, start with `~/.gateway/logs/http-exchange.jsonl`.
- Correlate request/response entries with the `x-proxy-request-id` header.
- If the exchange log only shows `backend_error` / `request failed`, inspect the gateway backend client path next; the log usually means transport or upstream connectivity failure, not an Anthropic validation error.
