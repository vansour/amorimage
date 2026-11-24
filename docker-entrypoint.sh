#!/bin/sh
set -e

# bind 挂载的目录可能由 Docker 以 root 创建，先修正属主再降权运行
if [ "$(id -u)" = "0" ]; then
    mkdir -p "$AMOR_DB_DIR" "$AMOR_STORAGE_DIR"
    chown -R amorimage:amorimage "$AMOR_DB_DIR" "$AMOR_STORAGE_DIR"
    exec setpriv --reuid amorimage --regid amorimage --init-groups /usr/local/bin/amorimage
fi

exec /usr/local/bin/amorimage
