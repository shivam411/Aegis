# Groups

Apps deployed from the same repository and branch can form a **group**: they
deploy together at one commit, from one clone, in order, and "Deploy changed
only" (and the group's webhook) deploys just the apps whose files changed.
How it works, and an example: [App groups](../groups.md).

## On the Overview

A group is one card with its apps inside: each app's status, live version
and, once updates have been checked, how many new commits concern it. The
buttons (deployers and admins):

- **Deploy group**: every app at the branch's latest commit.
- **Deploy changed only**: the apps whose watch paths changed since their
  live commit (apps never deployed go too).
- **Roll back group**: every app the latest group deployment switched goes
  back to its previous release.

While a group deployment runs, the card shows *Deploying* and each app's
progress (deploying, deployed, skipped, failed, rolled back). Apps outside
any group are shown as usual below.

**+ New group** (admins) opens a form: name, repository URL, branch,
whether to roll back on failure, and the apps already deployed from that
repository and branch to put in it.

## The group page

Click the group's name.

- **Apps**: the deploy order (↑ ↓ to change it), each app's live release
  and commit, its outcome in the last group deployment, and its **watch
  paths** (comma-separated globs such as `frontend_v2/**, shared/**`;
  **Save**). **Add app** adds another app from the same repository and
  branch; **Remove** takes one out (it is deployed on its own again).
- **Group deployments**: the latest runs with their commit and what each
  app did.
- **GitHub webhook** (admins): generate the group's secret, copy the
  payload URL, send a test delivery, see recent deliveries. While an app is
  in a group, its own webhook ignores pushes.
- **Group settings** (admins): name, repository, branch, roll back on
  failure, **Delete group** (the apps stay).

An app's page shows a **Group** link next to its status.
