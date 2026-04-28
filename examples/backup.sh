#!/bin/bash

ROOT_DS=(
    backups
    home
)

FOLDERS=(
backups
home/alice
home/rsync
home/bob
home/bob/media
home/bob/streaming
home/bob/immich
home/private
)

SOURCE_POOL=tank
BACKUP_POOL=backup
SNAPSHOT_TTL=1m

if [ -f /$BACKUP_POOL/present ]; then
   if [ ! -f /$BACKUP_POOL/running ]; then
	date > /$BACKUP_POOL/running
        date

        echo "starting backup…"

    for DS in "${ROOT_DS[@]}"; do
        if [ ! -d /$BACKUP_POOL/$DS ]; then
            zfs create $BACKUP_POOL/$DS
        fi
    done

    for F in "${FOLDERS[@]}"; do
        echo "backing up $F…"
        /usr/sbin/zfSnap -d -a $SNAPSHOT_TTL $SOURCE_POOL/$F
        echo ""
	echo "backup $F"
        /root/tools/zfs-backup/sync-zfs-snapshots \
        $SOURCE_POOL/$F $BACKUP_POOL/$F \
	--remove-on-destination
        echo "SOURCE: "
        zfs list -t snapshot -o creation,used,name $SOURCE_POOL/$F
        echo "BACKUP: "
        zfs list -t snapshot -o creation,used,name $BACKUP_POOL/$F
        echo ""
    done

    rm /$BACKUP_POOL/running
    echo "…done"
   fi
fi
