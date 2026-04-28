#!/bin/bash

POOL=backup
MARKER=3m

zfs list -t snapshot -o name | grep "$MARKER" | tac | xargs -n 1 zfs destroy -r
