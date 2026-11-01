# Jobs

**App → Settings → Jobs.** Commands run inside the app's **live release**, on a schedule or on demand: migrations, cleanups, reports.

- Runs with `sh -c` in the release folder, with the app's variables, env file and secrets (plus `AEGIS_JOB`, `AEGIS_RELEASE_VERSION`). Output (stdout and stderr) keeps the last 32 KB per run.
- Schedules are cron in **UTC**: `minute hour day-of-month month day-of-week`, e.g. `*/15 * * * *` or `0 3 * * 1-5`; or `@hourly`, `@daily`, `@weekly`, `@monthly`. Lists, ranges and steps work; Sunday is 0 or 7; if both day fields are set, either may match. The form has presets.
- A job past its **timeout** (default 3600 s, max 24 h) is killed with everything it started.
- Never started twice at once: a scheduled start while running is *skipped*; **Run now** errors.
- Scheduled runs need a live release, else *skipped*. Not available for container apps.
- Last 50 runs kept. A run interrupted by a daemon restart is marked failed.

| Button | Who | Does |
| :--- | :--- | :--- |
| **Run now** | deployer, admin | Starts it now. |
| **Runs** | everyone | History: status, exit code, duration, output tail. |
| **Pause / Resume** | admin | Stops or restarts the schedule. |
| **Delete** | admin | Removes the job and history. |
| **Add job** | admin | Name, schedule, command, optional timeout. |

Jobs are arbitrary code: creating, changing, deleting needs a signed-in admin. API tokens can only **run** jobs.

API: `GET`/`POST /projects/{p}/jobs`, `PUT`/`DELETE /projects/{p}/jobs/{id}`, `POST /projects/{p}/jobs/{id}/run`, `GET /projects/{p}/jobs/{id}/runs` ([HTTP API](../http_api.md)). Events: `JobCreated`, `JobUpdated`, `JobDeleted`, `JobRunStarted`, `JobRunFinished` (never the output).
