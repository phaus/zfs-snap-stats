# ZFS Backup Scripts

Example scripts for automated ZFS snapshot-based backups using
[zfSnap](https://www.zfsnap.org/) and
[sync-zfs-snapshots](https://github.com/phaus/sync-zfs-snapshots).

## Snapshot TTL Modifiers

| Modifier | Meaning |
|----------|---------|
| `y` | years (calendar) |
| `m` | months (calendar) |
| `w` | weeks |
| `d` | days |
| `h` | hours |
| `M` | minutes |
| `s` | seconds |
| `forever` | never expires |

## Scripts

- **setup.sh** -- installs zfSnap and sync-zfs-snapshots
- **backup.sh** -- creates snapshots and syncs them from `tank` to `backup` pool
- **cleanup.sh** -- removes old snapshots matching a TTL marker
- **stats.sh** -- shows pool status and I/O stats

## Cron Example

```cron
0 3 * * * /root/tools/zfs-backup/backup.sh >> /var/log/zfs-backup.log 2>&1
```
