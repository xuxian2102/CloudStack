#!/bin/sh
set -eu

if [ -z "${WAYLAND_DISPLAY:-}" ]; then
  printf '%s\n' 'CloudStack 只支持原生 Wayland；当前会话没有 WAYLAND_DISPLAY。' >&2
  exit 1
fi

export GDK_BACKEND=wayland
exec /usr/lib/cloudstack/blog-editor "$@"
