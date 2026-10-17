#!/bin/sh
# 命名卷挂进来时属主是 root，先改成运行用户再降权，设置才写得进去。
set -eu

config=/home/apw/.config/apple-pickup-watcher
mkdir -p "$config"
chown apw:apw /home/apw/.config "$config"
# setpriv 只改 uid，不改 HOME。入口以 root 启动时 HOME=/root，降权后
# 仍会去写 /root/.config，用户 apw 没有权限，设置就存不下来。
export HOME=/home/apw
export USER=apw
export LOGNAME=apw
exec setpriv --reuid=apw --regid=apw --init-groups --inh-caps=-all /usr/local/bin/apw-web
