# App groups

An **app group** is a set of apps deployed from the same repository and
branch, such as a monorepo with a front end, an API and an admin dashboard.
A group deploys them together:

- **One commit.** The branch's latest commit is resolved once (or you give
  one), and every app is built from it.
- **One clone.** The repository is cloned once per group deployment
  (`--filter=blob:none`, so only the files at that commit are downloaded),
  and each app's release is copied from that checkout. Every app still gets
  its own release directory and process, so releases stay isolated and each
  app can be rolled back on its own.
- **In order.** Apps deploy one after another in the group's order (say the
  API before the front end that calls it).
- **Only what changed.** With *only changed* (the default for "Deploy
  changed only" and for the group webhook), an app is skipped when none of
  the files changed between its live commit and the target commit match its
  **watch paths**. An app that has never been deployed always deploys.
- **Stop on failure.** When an app fails (build, or its health check), the
  apps after it are not deployed. With *roll back on failure* (on by
  default), the apps this group deployment already switched go back to the
  release they had before. The failed app itself keeps (or gets back) its
  previous release as any failed deployment does.

Only apps deployed from a repository can be in a group: an app deployed from
a folder on the server can't (its code doesn't come from a commit). An app is
in one group at most, and its repository and branch must be the group's
(GitHub URLs are compared as `github.com/owner/repo`, so `https://…` and
`git@…` forms match).

## Example: tripyog

One repository, `git@github.com:tripyog/tripyog.git`, with `frontend_v2/`,
`backend/`, `edashboard/` and `shared/`. Production deploys `main`, a
development copy deploys `develop`:

| Group | Branch | Apps (in order) | Watch paths |
| :--- | :--- | :--- | :--- |
| `tripyog` | `main` | `ty-back` | `backend/**`, `shared/**` |
| | | `ty-front` | `frontend_v2/**`, `shared/**` |
| | | `ty-edashboard` | `edashboard/**`, `shared/**` |
| `tripyog-dev` | `develop` | `ty-dev-back`, `ty-dev-front`, `ty-dev-edashboard` | the same |

1. Create the six apps (New app, Git repository, with **App folder**
   `backend`, `frontend_v2` or `edashboard`; branch `main` or `develop`),
   or import them with *Scan a folder*. Give one app of each branch a deploy
   key (Settings → Repository) and add it to the repository on GitHub; the
   group clones with the first member's deploy key it finds.
2. **Overview → + New group**: name `tripyog`, repository URL, branch
   `main`; tick `ty-back`, `ty-front`, `ty-edashboard`. Create.
3. On the group page, set each app's **watch paths** and order (↑ ↓).
4. **GitHub webhook** card on the group page: *Generate secret*, then in
   GitHub → Settings → Webhooks → Add webhook paste the Payload URL
   (`https://<aegis>/hooks/github/groups/<group id>`) and the secret,
   content type `application/json`, push events. Remove the apps' own
   webhooks from GitHub (they are ignored while the apps are in the group).
5. Repeat for `tripyog-dev` on `develop`.

From then on, a push to `main` that only touches `frontend_v2/` deploys
`ty-front` and skips the other two. A change in `shared/` deploys all three,
`ty-back` first.

The same with the API:

```sh
curl -sS -X POST "$AEGIS/api/v1/groups" -H 'Content-Type: application/json' \
  -b cookies -H "X-CSRF-Token: $CSRF" -d '{
  "name": "tripyog", "repository_url": "git@github.com:tripyog/tripyog.git", "branch": "main",
  "members": [
    {"project": "ty-back", "watch_paths": ["backend/**", "shared/**"]},
    {"project": "ty-front", "watch_paths": ["frontend_v2/**", "shared/**"]},
    {"project": "ty-edashboard", "watch_paths": ["edashboard/**", "shared/**"]}
  ]}'
curl -sS -X POST "$AEGIS/api/v1/groups/tripyog/deploy" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"only_changed": true}'
```

and the CLI: `aegis group list`, `aegis group show tripyog`,
`aegis group deploy tripyog [--all] [--commit SHA] [--detach]`,
`aegis group rollback tripyog`.

## Apps and watch paths

Watch paths are globs relative to the repository root:

- `*` and `?` match within one folder or file name, `**` across folders:
  `frontend_v2/**`, `apps/*/src/**`, `**/*.sql`.
- A plain path matches that file, or everything under that folder:
  `shared` is the same as `shared/**`; `package.json` is just that file.
- No watch paths: any change in the repository counts.

The files compared are those changed between the app's **live commit** (the
commit its live release was built from) and the target commit, so an app
that missed several pushes catches up on all of them. When the live commit
can't be found in the repository (say after a force-push), the app deploys.

## Webhook

Each group has its own GitHub webhook (secret, payload URL, recent
deliveries and a test delivery on the group page). A push to the group's
branch starts a group deployment of the changed apps at the pushed commit. A
redelivery (same delivery id) is reported as `duplicate`; a push of a commit
the group already deployed (or is deploying) is ignored.

While an app is in a group, pushes to **its own** webhook are answered with
`ignored` ("pushes deploy it through the group's webhook"), so a push that
reaches both webhooks deploys it once. Remove the app's own webhook from
GitHub, or leave it: it does nothing until the app leaves the group.
Scheduled daily deploys of a member app still deploy just that app.

## Rolling back

**Roll back group** (or `POST /api/v1/groups/{group}/rollback`, `aegis group
rollback`) puts every app the latest group deployment switched back on the
release it had before that deployment (recorded per app when the group
deployment ran). An app that had no release before is stopped. Apps that
were skipped are left alone. Each app can also be rolled back on its own from
its Releases tab.

## Events

`GroupCreated`, `GroupUpdated`, `GroupMembersChanged`, `GroupDeleted`,
`GroupDeploymentStarted`, `GroupCheckoutReady`, `GroupMemberSkipped`,
`GroupMemberDeploying`, `GroupMemberDeployed`, `GroupMemberFailed`,
`GroupMembersRolledBack`, `GroupRollbackStarted`, `GroupDeploymentFinished`
and `GroupDeploymentFailed`; see [event_catalog.md](event_catalog.md#groups).
Each member's own deployment has its usual events, with the trigger
`group:<name>`.

A group deployment interrupted by a daemon restart is marked failed on the
next start, like an interrupted deployment.
