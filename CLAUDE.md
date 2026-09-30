# easynews-nzb

A Newznab indexer plus SABnzbd-compatible download client backed by an Easynews account, in one
container. Read `README.md` (what and why), then `docs/DESIGN.md` (the specification, with the
verified Easynews API), then `LOCAL-DEPLOYMENT.md` (the founder's own network: hosts, paths, where
every credential lives, the exact wiring and test steps). `LOCAL-DEPLOYMENT.md` is git-ignored on
purpose: it describes a private network and must never be committed or pasted into a public place.

## Rules

- This repository is public on GitHub. Nothing that identifies the founder's network (addresses,
  host names, container ids, family names, file paths under `/root`) goes into tracked files.
  Generic examples use `192.0.2.x` and `<host>`.
- Secrets never appear in the chat, in commits, in logs or in tests: no passwords, no API keys, no
  Easynews `sig` or `sid` values. Mask them in debug output.
- No AI attribution anywhere: no `Co-Authored-By` trailers, no "generated with" lines, in commits,
  code, comments or docs.
- Kill processes by PID only. Do not touch firewalls.
- Work through `LOCAL-DEPLOYMENT.md` top to bottom; every step has a check, do not skip checks.
- When the deployment works, update the founder's infrastructure notes as `LOCAL-DEPLOYMENT.md`
  section 5.5 says, and mark this repository's README status line as implemented.

## Layout (target)

```
app/            FastAPI application: newznab.py, nzb.py, sab.py, health.py, easynews.py, jobs.py, state.py
Dockerfile      python:3.12-slim, non-root user, uvicorn on 8090
docker-compose.example.yml
docs/DESIGN.md
tests/          unit tests with recorded (masked) Easynews responses; no live calls in CI
```
