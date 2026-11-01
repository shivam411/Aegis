# Groups

Apps from the same repository and branch can form a **group**: one commit, one clone, deployed in order. "Deploy changed only" (and the group webhook) deploys just apps whose files changed. Details and example: [App groups](../groups.md).

## On the Overview

A group is one card with its apps, each showing status, live version and new commits concerning it. Buttons (deployers and admins):

- **Deploy group**: every app at the branch's latest commit.
- **Deploy changed only**: apps whose watch paths changed since their live commit (never-deployed apps go too).
- **Roll back group**: apps switched by the latest group deployment return to their previous release.

During a run the card shows each app's progress (deploying, deployed, skipped, failed, rolled back). Ungrouped apps appear below.

**+ New group** (admins): name, repository URL, branch, roll back on failure, and the apps to include.

## The group page

Click the group's name.

- **Apps**: deploy order (↑ ↓), live release and commit, last outcome, and **watch paths** (comma-separated globs, e.g. `frontend_v2/**, shared/**`; **Save**). **Add app** (same repository and branch). **Remove** deploys it on its own again.
- **Group deployments**: recent runs and what each app did.
- **GitHub webhook** (admins): generate secret, copy payload URL, send test, see deliveries. Grouped apps' own webhooks ignore pushes.
- **Group settings** (admins): name, repository, branch, roll back on failure, **Delete group** (apps stay).

An app's page shows a **Group** link next to its status.
