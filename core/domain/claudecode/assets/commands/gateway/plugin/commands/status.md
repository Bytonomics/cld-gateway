A JSON object describing the current cld-gateway status is appended below this
instruction. It was produced by the gateway itself (including a live call to
the OpenAI Codex usage endpoint when credentials were available) - it is real,
current data, not something you need to look up or estimate.

Render it as a concise status report for the user, covering:

- Gateway: version, config path, current working directory (the gateway object).
- Session: thread id/name and account display, when present (the session object).
- Model: the requested and resolved model, and reasoning effort (the model object).
- Provider: the backend base URL and account id (the provider and auth objects).
- Usage: plan type, primary/secondary rate-limit windows (used percent, reset
  time), any additional named rate limits, and spend-control status (the
  plan_type, rate_limits, and spend_control fields).

Formatting rules:

- Use only fields present in the JSON. Never invent, estimate, or guess a value
  that is null or absent - state plainly that it is unavailable instead.
- If usage_state is stale_or_unavailable, say so explicitly and omit the
  usage/rate-limit section rather than filling it with placeholder data.
- This is not a background-job or task-queue system - do not render a job
  table, job IDs, or any "no jobs recorded" style output; there is no such
  concept here.
- A short labeled list or small table of the fields above is preferred over
  prose.

JSON status data:
