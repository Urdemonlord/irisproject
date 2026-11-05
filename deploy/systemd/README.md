# IRIS systemd units

## `iris-api.service` (live, NOT in this repo)

The production API unit lives only on the host (`/etc/systemd/system/iris-api.service`):
`User=meowlabs`, `WorkingDirectory=/home/meowlabs/projects/irisproject`,
`EnvironmentFile=.../.env`, `ExecStart=/usr/local/bin/iris-api`, `Restart=always`.
It is documented here so the safety-net units below can mirror it exactly.

## `iris-cleanup.service` + `iris-cleanup.timer` (safety net, NOT enabled)

### Why

The retention job (expire temp inboxes, purge their mail via JMAP, prune old
sessions and rate-limit events) runs in-process inside the API every 60 s
(`apps/api/src/cleanup.rs`). If the API is down, nothing expires: temp-inbox
mail piles up on Stalwart and stale rows stay in SQLite. The timer is the
out-of-process backstop for exactly that case.

### How it works

- `iris-cleanup.service` runs `/usr/local/bin/iris-api cleanup-once` as user
  `meowlabs` with the same working directory and `.env` as the API, so it
  reads the same database and mail credentials. No new daemon, no new
  endpoint, no new credentials: one pass of the existing cleanup code, then
  the process exits.
- `iris-cleanup.timer` fires it every 15 minutes (`Persistent=true`, so a
  missed run during host downtime fires once on boot).
- Overlapping runs with a live API are safe: `mark` is a conditional UPDATE
  and the JMAP destroys only count ids the server reports destroyed, so two
  passes cannot double-delete or resurrect rows.

### Activation (needs a separate approval — do NOT enable as part of a code deploy)

Prerequisite: the deployed binary at `/usr/local/bin/iris-api` must include
the `cleanup-once` subcommand (IRIS debt round 2 or later). Check first:

```bash
# One manual pass; exit code 0 means the subcommand exists and the pass ran.
# It performs real (idempotent) cleanup work, so run it once and check the log.
sudo -u meowlabs /usr/local/bin/iris-api cleanup-once; echo "exit=$?"
```

Then install and enable the timer:

```bash
sudo cp deploy/systemd/iris-cleanup.service deploy/systemd/iris-cleanup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now iris-cleanup.timer
```

Verify:

```bash
systemctl list-timers iris-cleanup.timer
systemctl status iris-cleanup.service
journalctl -u iris-cleanup.service -n 20   # expect a `cleanup marked=…` line when work was done
```

### Rollback / disable

```bash
sudo systemctl disable --now iris-cleanup.timer
sudo rm /etc/systemd/system/iris-cleanup.service /etc/systemd/system/iris-cleanup.timer
sudo systemctl daemon-reload
```

Disabling only removes the safety net; the in-process 60 s job in the API is
unaffected.
