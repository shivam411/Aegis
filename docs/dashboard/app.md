# App page

Click an app's name anywhere to open its page (`#/apps/<name>`). **← All apps** goes back to the [Overview](overview.md).

![The app page with the Deploy tab open](../images/dashboard/app-deploy.png)

## Header

- **Name and status.** The same status pill as on the Overview. Docker apps also get a blue **Docker** pill; hover it for the image name (see [Docker apps](docker-and-instances.md#docker-apps)).
- **Start / Stop / Restart.** Process control, for deployers and admins. Stop and Restart ask for confirmation; with several instances, Restart replaces them one at a time so the others keep serving.
- **Subtitle.** The live version, the number of instances (when more than one), uptime, restart count, the process id (`container pid` for Docker apps), the Docker image, and the source.

The header refreshes by itself when anything about the app changes.

## Tabs

| Tab | Key | What it shows |
| :--- | :--- | :--- |
| [Deploy](deploy.md) | <kbd>1</kbd> | Start a deployment; the latest deployment's pipeline; the instances. |
| [Releases](releases.md) | <kbd>2</kbd> | Every release kept on disk; rollback. |
| [Logs](logs.md) | <kbd>3</kbd> | Live process output with search and download. |
| [Resources](resources.md) | <kbd>4</kbd> | CPU and memory now and over time; limits. |
| [Settings](settings.md) | <kbd>5</kbd> | Build and run settings, domains, secrets, daily deploy, webhook, facts. |
| [Activity](activity.md) | <kbd>6</kbd> | This app's audit log. |

Each tab has its own address (for example `#/apps/shop/logs`), so you can bookmark or share it.

## Who can do what here

| | Viewer | Deployer | Admin |
| :--- | :---: | :---: | :---: |
| See every tab | ✓ | ✓ | ✓ |
| Start, stop, restart | | ✓ | ✓ |
| Deploy, roll back | | ✓ | ✓ |
| Change settings, domains, secrets, schedule, webhook, limits | | | ✓ |
| View `aegis.toml` | ✓ | ✓ | ✓ |
