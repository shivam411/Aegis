# Jobs

**App → Settings → Jobs.** Commands that run inside the app's **live release**, on a schedule or on demand: database migrations, cleanups, reports, cache warm-ups.

- A job runs with `sh -c` in the release's folder, with the app's variables, env file and secrets (and `AEGIS_JOB`, `AEGIS_RELEASE_VERSION`). Output (stdout and stderr together) is kept, the last 32 KB of each run.
- Schedules are cron expressions in **UTC**: `minute hour day-of-month month day-of-week`, e.g. `*/15 * * * *` (every 15 minutes), `0 3 * * 1-5` (03:00 on weekdays), or `@hourly`, `@daily`, `@weekly`, `@monthly`. Lists (`1,15`), ranges (`1-5`) and steps (`*/10`) work; Sunday is 0 or 7; when both day fields are restricted, either may match (as in cron). The form has presets.
- A job that outlives its **timeout** (default 3600 s, up to 24 h) is killed together with everything it started.
- A job is never started twice at once: a scheduled start while it still runs is recorded as *skipped*; **Run now** answers with an error.
- A scheduled run needs a live release; without one it is recorded as *skipped*. Jobs are not available for container apps yet.
- The last 50 runs of each job are kept. If the daemon restarts during a run, that run is marked failed.

| Button | Who | Does |
| :--- | :--- | :--- |
| **Run now** | deployer, admin | Starts the job immediately. |
| **Runs** | everyone | The history: status, exit code, duration, output tail. |
| **Pause / Resume** | admin | Stops or restarts the schedule. |
| **Delete** | admin | Removes the job and its history. |
| **Add job** | admin | Name, schedule, command, optional timeout. |

A job is arbitrary code, so creating, changing and deleting jobs needs a signed-in admin; an API token can **run** existing jobs (for a deploy script, say) but not create them.

API: `GET`/`POST /projects/{p}/jobs`, `PUT`/`DELETE /projects/{p}/jobs/{id}`, `POST /projects/{p}/jobs/{id}/run`, `GET /projects/{p}/jobs/{id}/runs` ([HTTP API](../http_api.md)). Events: `JobCreated`, `JobUpdated`, `JobDeleted`, `JobRunStarted`, `JobRunFinished` (never the output).
