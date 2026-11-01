# Docker

Containers on the server and their logs. It is shared separately from apps: **only the owner can see it**, plus users the owner allows.

The **Docker** link in the top bar (<kbd>g</kbd> <kbd>c</kbd>) is hidden for everyone else.

## Who can see it

- The **owner** is the first admin account. Always allowed.
- Anyone else, including other admins, sees nothing until you tick **Docker** for them in [Account → Users](account.md#users) (or when adding the user).
- API tokens can never use it.

## Using it

The table lists every container: name, image, state, ports and age. Containers started by Aegis for an app have an **Aegis app** badge.

Click a row to open its logs. They refresh every 3 seconds and have the same filters as [app logs](logs.md): search, regex, level chips and time range.

If Docker isn't installed or the daemon isn't reachable, the page says why.
