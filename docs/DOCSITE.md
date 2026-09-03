---
type: plan
title: "Plan: Documentation Site Layout"
status: superseded
tags:
  - plan
  - docs-site
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Plan: Documentation Site Layout

| Section | What it covers |
|---------|----------------|
| [Outcome](#outcome) | Why this plan is superseded and what replaced it |
| [Path mapping](#path-mapping) | Where each originally-planned page ended up |

## Outcome

This plan proposed a `site/` tree (modeled on Docusaurus-style Getting Started -> Concepts ->
Reference arc, two audiences: users and contributors) as a docs staging area, to be lifted
into a real static site later. That tree existed briefly at docs/site/ and has since been
superseded: the project adopted `docsmith` governance instead, which classifies every doc into
one of its own categories (tutorial, how-to, reference, explanation, runbook, decision) placed
directly under `docs/`, so no separate staging tree is needed -- `docsmith collect` builds the
site straight from those category folders.

## Path mapping

| Originally planned (defunct, no longer on disk) | Current location |
|---|---|
| site/index.md | `docs/explanation/overview.md` |
| site/docs/getting-started/installation.md | `docs/tutorials/installation.md` |
| site/docs/getting-started/quickstart.md | `docs/tutorials/quickstart.md` |
| site/docs/getting-started/setup-command.md | `docs/tutorials/setup-command.md` |
| site/docs/configuration/index.md | `docs/reference/configuration/index.md` |
| site/docs/configuration/backends.md | `docs/reference/configuration/backends.md` |
| site/docs/configuration/logs-and-state.md | `docs/reference/configuration/logs-and-state.md` |
| site/docs/usage/commands.md | `docs/reference/commands.md` |
| site/docs/usage/api.md | `docs/reference/api.md` |
| site/docs/usage/troubleshooting.md | `docs/runbooks/troubleshooting.md` |
| site/docs/usage/security.md | `docs/explanation/security.md` |
| site/contributing/architecture.md | `docs/explanation/architecture.md` |
| site/contributing/design-decisions.md | `docs/explanation/design-decisions.md` |
| site/contributing/adr/* | `docs/decisions/` (ADR-NNNN-slug.md) |
| site/contributing/extending/backends.md | `docs/how-to/extending-backends.md` |
| site/contributing/extending/translators.md | `docs/how-to/extending-translators.md` |
| site/contributing/testing.md | `docs/how-to/testing.md` |
| site/contributing/releasing.md | `docs/runbooks/releasing.md` |
