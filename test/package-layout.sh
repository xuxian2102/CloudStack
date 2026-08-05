#!/usr/bin/env bash
set -euo pipefail

package_archive=${1:?用法: test/package-layout.sh <cloudstack-package.tar.zst>}
if [[ ! -f "$package_archive" ]]; then
  echo "找不到 Arch 包：$package_archive" >&2
  exit 1
fi

contents=$(bsdtar -tf "$package_archive")
for expected in \
  usr/bin/cloudstack \
  usr/lib/cloudstack/cloudstack \
  usr/share/applications/dev.xuxian.cloudstack.desktop \
  usr/share/icons/hicolor/512x512/apps/dev.xuxian.cloudstack.png \
  usr/share/icons/hicolor/scalable/apps/dev.xuxian.cloudstack.svg; do
  if ! grep -Fxq "$expected" <<<"$contents"; then
    echo "Arch 包缺少：$expected" >&2
    exit 1
  fi
done

if grep -Eq '(^|/)(blog-editor|dev\.xuxian\.blogeditor)(/|\.|$)' <<<"$contents"; then
  echo "Arch 包仍安装旧二进制或旧 App ID" >&2
  grep -E '(^|/)(blog-editor|dev\.xuxian\.blogeditor)(/|\.|$)' <<<"$contents" >&2
  exit 1
fi

echo "Arch 包布局检查通过"
