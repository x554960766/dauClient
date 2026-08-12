#!/bin/sh
# sync-local-bundle.sh — 将本地已下载的 Android SDK（含系统镜像）同步到 src-tauri/sdk-bundle/
# 供 Tauri 打包为离线开箱即用的安装包（无需网络下载 2GB+ 资源）

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUNDLE_DIR="$PROJECT_ROOT/src-tauri/sdk-bundle"

# 默认 Mac 本地 SDK 缓存目录
DEFAULT_LOCAL_SDK="$HOME/Library/Caches/umeng-dau-client/sdk"
LOCAL_SDK="${1:-$DEFAULT_LOCAL_SDK}"

if [ ! -d "$LOCAL_SDK" ]; then
  echo "错误: 本地 SDK 目录不存在: $LOCAL_SDK"
  exit 1
fi

echo "========================================"
echo "  同步本地 SDK 到 Bundle 目录"
echo "========================================"
echo "源 SDK 目录:   $LOCAL_SDK"
echo "目标 Bundle:   $BUNDLE_DIR"
echo ""

# 1. 清理旧 bundle（保留 .gitkeep）
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR"
touch "$BUNDLE_DIR/.gitkeep"

# 2. 复制必需组件
COMPONENTS="cmdline-tools jre platform-tools emulator build-tools licenses system-images"

for item in $COMPONENTS; do
  SRC_PATH="$LOCAL_SDK/$item"
  if [ -e "$SRC_PATH" ]; then
    echo "==> 同步 $item ..."
    cp -R "$SRC_PATH" "$BUNDLE_DIR/"
  else
    echo "警告: 源码缺失组件 $item (跳过)"
  fi
done

# 3. 清理无用文件与临时下载缓存
echo "==> 清洗临时文件 (.dl, .staging, .temp, .DS_Store)..."
find "$BUNDLE_DIR" -name ".DS_Store" -delete 2>/dev/null || true
find "$BUNDLE_DIR" -name ".dl" -exec rm -rf {} + 2>/dev/null || true
find "$BUNDLE_DIR" -name ".staging" -exec rm -rf {} + 2>/dev/null || true
find "$BUNDLE_DIR" -name ".temp" -exec rm -rf {} + 2>/dev/null || true

# 4. 设置 UNIX 可执行权限
echo "==> 修复可执行文件权限..."
chmod -R +x "$BUNDLE_DIR/cmdline-tools/latest/bin/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/platform-tools/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/emulator/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/build-tools/34.0.0/"* 2>/dev/null || true
if [ -d "$BUNDLE_DIR/jre/Contents/Home/bin" ]; then
  chmod -R +x "$BUNDLE_DIR/jre/Contents/Home/bin/"* 2>/dev/null || true
elif [ -d "$BUNDLE_DIR/jre/bin" ]; then
  chmod -R +x "$BUNDLE_DIR/jre/bin/"* 2>/dev/null || true
fi

# 5. 汇总大小
echo ""
echo "========================================"
echo "  Bundle 同步完成 (全离线/开箱即用)"
echo "========================================"
du -sh "$BUNDLE_DIR"
echo ""
echo "已包含内容:"
ls -lh "$BUNDLE_DIR"
echo ""
echo "下一步: cd src-tauri && cargo tauri build"
echo "  打包后的应用将包含上述完整工具链与系统镜像，首次运行秒级解压即用。"
