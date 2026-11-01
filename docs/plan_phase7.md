# Plan: Phase 7 (access, safer releases, ecosystem, fleet)

Status: **in progress** (see the progress table below). This plan follows the [public roadmap](public_roadmap.md) and the detailed [web control plane roadmap](roadmap_web_control_plane.md). Pick milestones in order; each one ships on its own and has a "done when" check. Sizes: **S** ≈ days, **M** ≈ 1–2 weeks, **L** ≈ several weeks.

Principles (unchanged): one binary, embedded SQLite, no required external services; every feature is optional and off by default; every change is an audited event; nothing is called validated without a CI test.

## Progress

| Item | State |
| :--- | :--- |
| M0: app lists, hidden apps, list view, bulk actions, paging, `agent.md` | ✅ Shipped ([PR 27](https://github.com/shivam411/Aegis/pull/27)) |
| M0: server-side paging (`limit`, `offset`, `X-Total-Count`), groups can't reference hidden apps, bulk rollback | ✅ Done |
| M0: mobile test of the new screens | ⬜ Not done |
| M2: live URL check after promotion, rollback that keeps the HTTP status or error, on-demand check | ✅ Done (experimental) |
| M2: deploy hooks, approval gates, canary and traffic split, preview environments | ⬜ Not started |
| M3: Prometheus `/metrics` and Grafana dashboard, on demand | ✅ Done (experimental); request counts per app need proxy counters first |
| M3: more alert channels, alert rules, log shipping, status page | ⬜ Not started |
| M5: jobs (cron and one-off commands) | ✅ Done (experimental) |
| M5: managed add-ons, buildpacks, scale to zero, autoscaling | ⬜ Not started (see "Why M5 is split") |
| Subdomain quick-add in the dashboard | ✅ Done |

**Why M5 is split.** Add-ons (Postgres, Redis) need a non-HTTP health check (`pg_isready` through `docker exec`), backups of their volumes and a way to hand connection strings to an app. Buildpacks need a builder (Nixpacks or `pack`) installed and a CI deploy test per language. Scale to zero and autoscaling both need per-app request counts in the built-in proxy, which would also feed canary deploys and Prometheus. Each is its own change.

---

## Milestone 0: finish what is open (S)

| Item | Done when |
| :--- | :--- |
| Merge [PR #27](https://github.com/shivam411/Aegis/pull/27) (app lists, hidden apps, list view, bulk actions, paging, `agent.md`) | CI green on `cargo test --workspace`, `cargo check --benches`, Playwright (desktop and mobile) |
| Mobile check of the new screens (list view, bulk bar, pager, Users apps picker) | `ui-tests/tests/mobile.spec.js` covers them |
| Server-side paging for `/deployments` and `/events` (`limit`, `offset`, total count header) | A server with 100k events loads the Events page without fetching them all |
| Groups can't reference apps the caller can't see | A test creates a group as an admin who hasn't unlocked an app and gets 404 |
| Bulk actions for groups (deploy/rollback several groups) and bulk Rollback | UI test |

## Milestone 1: stronger access control (M)

Why: today's limits only apply to signed-in sessions. Staff sharing and CI need finer scopes.

1. **Per-app API tokens.** A token can be limited to a list of apps and a scope (`read` / `deploy`). Reuses the `Visibility` check from `core/web/src/access.rs` for tokens.
2. **Two-factor sign-in (TOTP)** per user, with recovery codes; admins can require it for everyone.
3. **OIDC single sign-on** (Google, GitHub, Entra ID, Keycloak): a configured issuer maps to a role and an app list. Local accounts stay as the fallback.
4. **Per-user app list by group/team**: a "team" is a named list of apps that users join, so adding an app updates every member at once.
5. **Session management**: list and end your own sessions; admins can end any user's sessions.
6. **Audit export**: `GET /events` as NDJSON with a cursor, plus an optional syslog/webhook sink.

Done when: a token limited to `wayog-api` can deploy it and gets 404 on every other app; a user with TOTP can't sign in with the password alone; an OIDC user lands with the mapped role and apps; each has a test in `core/web/tests/`.

## Milestone 2: safer releases (M–L)

1. **Canary deploys** behind the proxy: send 5–50% of requests to the new release for a set time, watch the error rate and latency the proxy already measures, then promote or roll back automatically.
2. **Traffic split and pinned release URLs** (`<version>.<app-domain>`), for manual checks before promoting.
3. **Deploy hooks**: `pre_deploy`, `post_deploy` and `on_failure` commands in `aegis.toml` (migrations, cache warm-up, notifications).
4. **Approval gates**: a deploy to an app marked "protected" waits for an admin or deployer other than the requester to approve it.
5. **Health-based auto-rollback after promotion**: roll back if the error rate stays above a threshold for N minutes.
6. **Preview environments** per branch or pull request (`pr-42.app.example.com`), created by the GitHub webhook and removed when the PR closes.

Done when: a test deploys a release that returns 5xx for 20% of requests and the canary rolls back with no more than the canary share of requests failing; hooks run in order and a failing `pre_deploy` stops the deploy.

## Milestone 3: observability and alerts (M)

1. **Prometheus `/metrics`** (token-protected): per-app process, restart, request count/latency/error counters from the proxy, deployment durations, host gauges. A ready Grafana dashboard in `examples/`.
2. **More alert channels**: email (SMTP), Discord, Telegram, Microsoft Teams, PagerDuty and a generic signed webhook; per-app routing and quiet hours.
3. **Alert rules in the dashboard**: error rate, latency p95, memory near limit, disk, certificate expiry, failed deploy.
4. **Log shipping** (optional): forward app logs to Loki, an S3 bucket or any HTTP endpoint.
5. **Status page** (optional, public): an app's up/down history on a separate path.

Done when: a Prometheus scrape of a test daemon shows the app's request counters, and a forced failure produces one alert in each configured channel (against local fakes in CI).

## Milestone 4: storage, secrets and DNS integrations (M)

Cloud providers and independent hosts are all optional plugins built on the event bus and the existing `plugin` crate.

| Area | What | Providers |
| :--- | :--- | :--- |
| Backups | Scheduled upload of backups to S3-compatible storage; retention rules; restore from a bucket | AWS S3, GCS, Azure Blob, Backblaze B2, Cloudflare R2, Wasabi, MinIO |
| Secrets | Resolve `secret://` references at start time; no copy stored in Aegis | AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, HashiCorp Vault, 1Password Connect |
| DNS and certificates | Create records when a domain is mapped; DNS-01 challenge for wildcard certificates | Cloudflare, Route 53, Cloud DNS, Azure DNS, DigitalOcean, Hetzner DNS |
| Registries | Pull private images for Docker apps; push built images | Docker Hub, GHCR, ECR, GCR/Artifact Registry, ACR |

Done when: a backup lands in a MinIO container in CI and restores onto a fresh data directory; a `secret://` value from a fake Vault reaches the app but never the database or events.

## Milestone 5: add-ons and builds (L)

1. **Managed add-ons**: one-click Postgres, MySQL, Redis and MinIO as Docker apps with a volume, scheduled backups (Milestone 4), and connection strings injected into the linked app as secrets.
2. **Buildpacks**: build without a Dockerfile (Nixpacks or Cloud Native Buildpacks), so Python, Go, Rust, Ruby and Java apps deploy as easily as Node. Includes CI deploy tests for each runtime (today only Node is deployed in CI).
3. **Cron jobs and one-off tasks** as workloads: scheduled commands with logs, history and alerts; "run once" for migrations.
4. **Scale to zero / wake on request** for idle apps, using the proxy to hold the first request.
5. **Request-based autoscaling** of instances between a minimum and maximum.

Done when: a Postgres add-on survives an Aegis restart and a restore test; a Python and a Go example deploy in CI through the buildpack path.

## Milestone 6: multi-node fleet (L)

1. **Node agent**: a small `aegis-agent` that a main Aegis controls over mutual TLS; same API surface for deploys, logs, metrics and process control.
2. **Add a server by SSH** from the dashboard (bootstrap with `install.sh --server`).
3. **Server selector** in the dashboard and CLI (`aegis --node web-2 ...`); apps pinned to a node, or replicated across nodes behind the controller's proxy.
4. **Fleet updates**: roll Aegis upgrades through nodes one at a time.
5. **Optional VPS provisioning** through provider APIs (Hetzner, DigitalOcean, Vultr, Linode, AWS EC2, GCP Compute, Azure VMs).

Done when: two nodes in CI (containers) serve one app, one node is killed, and the controller reports it and keeps the other serving.

## Milestone 7: proof and hardening (continuous)

Carried over from [proof_phase.md](proof_phase.md): published 24 h+ soak, blocking benchmark thresholds, Aegis deploying itself, tested PM2/systemd migration, macOS and Windows test runs, `install.sh --server` run in CI, and certificate issuance against a real (staging) ACME CA. Every new milestone adds its rows to [feature_maturity.md](feature_maturity.md) as 🧪 Experimental until a CI test backs them.

---

## Suggested order and dependencies

```
M0 ──► M1 (access) ──► M3 (observability) ──► M2 (canary needs error-rate metrics)
        │                                       │
        └────────► M4 (integrations) ──► M5 (add-ons need backups) ──► M6 (fleet)
                                   M7 runs alongside everything
```

First slice to start (about 3–4 weeks): M0, then per-app API tokens and TOTP from M1, S3 backups from M4, and Prometheus `/metrics` plus email/Discord/Telegram alerts from M3.

## Risks and open questions

- **Scope creep toward a full PaaS.** Keep each integration a plugin; the core stays small.
- **Security surface:** OIDC, tokens and hide passwords all depend on the single `Visibility` check; keep it the only place that decides what a caller sees and test every new endpoint against it.
- **Canary needs request metrics per release.** The proxy has them per app today; per release is a prerequisite.
- **Fleet changes the trust model** (the controller holds credentials for every node). Decide on mutual TLS and a per-node token before building it.
- **Questions for you:** which providers do you actually use (decides the order in Milestone 4)? Is a public status page wanted? Should hidden apps also hide from API tokens (a per-token option)?
