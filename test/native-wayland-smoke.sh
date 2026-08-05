#!/usr/bin/env bash
set -euo pipefail

for command in sway swaymsg dbus-run-session; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "缺少 Wayland smoke 依赖：$command" >&2
    exit 1
  fi
done

workspace_root=$(realpath "$(dirname "${BASH_SOURCE[0]}")/..")
app_binary=${1:-target/debug/cloudstack}
if [[ "$app_binary" != /* ]]; then
  app_binary="$workspace_root/$app_binary"
fi
if [[ ! -x "$app_binary" ]]; then
  echo "找不到可执行文件：$app_binary" >&2
  exit 1
fi

runtime_dir=$(mktemp -d "${TMPDIR:-/tmp}/cloudstack-wayland.XXXXXX")
chmod 700 "$runtime_dir"
compositor_pid=""
app_pid=""
cleanup() {
  if [[ -n "$app_pid" ]]; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  if [[ -n "$compositor_pid" ]]; then
    kill "$compositor_pid" 2>/dev/null || true
    wait "$compositor_pid" 2>/dev/null || true
  fi
  rm -r "$runtime_dir"
}
trap cleanup EXIT

sway_config="$runtime_dir/sway.conf"
printf '%s\n' \
  'xwayland disable' \
  'swaybg_command -' \
  'seat seat0 fallback true' \
  'output * mode 1280x800' >"$sway_config"
env -u DISPLAY -u WAYLAND_DISPLAY \
  XDG_RUNTIME_DIR="$runtime_dir" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
  sway --unsupported-gpu --config "$sway_config" \
  >"$runtime_dir/sway.log" 2>&1 &
compositor_pid=$!

wayland_socket=""
for _attempt in {1..200}; do
  for candidate in "$runtime_dir"/wayland-*; do
    if [[ -S "$candidate" ]]; then
      wayland_socket=$(basename "$candidate")
      break 2
    fi
  done
  kill -0 "$compositor_pid" 2>/dev/null || break
  sleep 0.05
done
if [[ -z "$wayland_socket" ]]; then
  sed -n '1,200p' "$runtime_dir/sway.log" >&2
  exit 1
fi

sway_socket=""
for candidate in "$runtime_dir"/sway-ipc.*.sock; do
  if [[ -S "$candidate" ]]; then
    sway_socket="$candidate"
    break
  fi
done
if [[ -z "$sway_socket" ]]; then
  echo "找不到 Sway IPC socket" >&2
  exit 1
fi

env -u DISPLAY \
  XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
  XDG_SESSION_TYPE=wayland GDK_BACKEND=wayland WEBKIT_DISABLE_DMABUF_RENDERER=1 \
  XDG_CACHE_HOME="$runtime_dir/cache" XDG_CONFIG_HOME="$runtime_dir/config" \
  XDG_DATA_HOME="$runtime_dir/data" \
  CLOUDSTACK_E2E_PROJECT="$workspace_root/fixtures/test-blog" \
  CLOUDSTACK_E2E_OPEN_FIRST=1 \
  CLOUDSTACK_E2E_DATA_DIR="$runtime_dir/app-data" \
  dbus-run-session -- "$app_binary" >"$runtime_dir/app.log" 2>&1 &
app_pid=$!

window_seen=0
for _attempt in {1..300}; do
  kill -0 "$app_pid" 2>/dev/null || break
  tree=$(swaymsg -s "$sway_socket" -r -t get_tree 2>/dev/null || true)
  if printf '%s' "$tree" | grep -Fq 'dev.xuxian.blogeditor'; then
    echo "检测到旧 GTK App ID" >&2
    exit 1
  fi
  if printf '%s' "$tree" | grep -Fq 'dev.xuxian.cloudstack'; then
    window_seen=1
    break
  fi
  sleep 0.05
done
if ((window_seen == 0)); then
  echo "原生 GTK 窗口没有出现在隔离 Wayland 会话中" >&2
  sed -n '1,240p' "$runtime_dir/app.log" >&2
  exit 1
fi

sleep 1
swaymsg -s "$sway_socket" '[app_id="dev.xuxian.cloudstack"] kill' >/dev/null
for _attempt in {1..100}; do
  kill -0 "$app_pid" 2>/dev/null || break
  sleep 0.05
done
if kill -0 "$app_pid" 2>/dev/null; then
  echo "关闭请求后应用仍在运行" >&2
  exit 1
fi

app_status=0
wait "$app_pid" || app_status=$?
app_pid=""
if ((app_status != 0)); then
  echo "应用异常退出：status=$app_status" >&2
  sed -n '1,240p' "$runtime_dir/app.log" >&2
  exit 1
fi
if grep -Eiq 'panic|segmentation fault|Gtk-CRITICAL|WebKit.*CRITICAL' "$runtime_dir/app.log"; then
  sed -n '1,240p' "$runtime_dir/app.log" >&2
  exit 1
fi

echo "原生 GTK4/WebKitGTK Wayland smoke 通过"
