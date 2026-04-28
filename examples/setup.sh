#!/bin/bash

apt update
apt install wget zfsnap
wget \
-O /root/tools/zfs-backup/sync-zfs-snapshots \
https://raw.githubusercontent.com/phaus/sync-zfs-snapshots/master/sync-zfs-snapshots
chmod +x /root/tools/zfs-backup/sync-zfs-snapshots
