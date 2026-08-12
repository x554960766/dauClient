#!/bin/sh
# prepare-bundle-win-cross.sh — 在 macOS 开发机上预下载 Windows 专属的 SDK 工具链与系统镜像
# 生成供 Windows 环境构建打包使用的 src-tauri/sdk-bundle/
#
# 用法：
#   cd umeng-dau-client && sh scripts/prepare-bundle-win-cross.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUNDLE_DIR="$PROJECT_ROOT/src-tauri/sdk-bundle"
TMP="$(mktemp -d /tmp/umeng-bundle-win.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

SYS_IMAGE="system-images;android-34;google_apis;x86_64"

echo "========================================"
echo "  在 macOS 上交叉准备 Windows SDK Bundle"
echo "========================================"
echo "Target Platform: Windows x64"
echo "Target Image:    $SYS_IMAGE"
echo "Bundle Dir:      $BUNDLE_DIR"
echo ""

# 清理旧 bundle
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR"
touch "$BUNDLE_DIR/.gitkeep"

# ---- 1. Windows cmdline-tools ----
echo "==> [1/5] 下载 Windows cmdline-tools..."
curl -fSL -o "$TMP/cmdline.zip" \
  "https://dl.google.com/android/repository/commandlinetools-win-11076708_latest.zip"
mkdir -p "$TMP/cmdline-extract"
unzip -q "$TMP/cmdline.zip" -d "$TMP/cmdline-extract"
mkdir -p "$BUNDLE_DIR/cmdline-tools"
mv "$TMP/cmdline-extract/cmdline-tools" "$BUNDLE_DIR/cmdline-tools/latest"

# ---- 2. Windows JRE x64 ----
echo "==> [2/5] 下载 Windows JRE (Temurin 17 x64)..."
curl -fSL -o "$TMP/jre.zip" \
  "https://api.adoptium.net/v3/binary/latest/17/ga/windows/x64/jre/hotspot/normal/eclipse"
mkdir -p "$TMP/jre-extract"
unzip -q "$TMP/jre.zip" -d "$TMP/jre-extract"
mv "$TMP/jre-extract"/jdk-* "$BUNDLE_DIR/jre"

# ---- 3. Windows platform-tools (含 adb.exe) ----
echo "==> [3/5] 下载 Windows platform-tools..."
curl -fSL -o "$TMP/platform-tools.zip" \
  "https://dl.google.com/android/repository/platform-tools-latest-windows.zip"
unzip -q "$TMP/platform-tools.zip" -d "$BUNDLE_DIR"

# ---- 4. license hash 直写 ----
echo "==> [4/5] 写 license 文件..."
mkdir -p "$BUNDLE_DIR/licenses"
printf '8933bad161af4178b1185d1a37fbf41ea5269c55\nd56f5187479451eabf01fb78af6dfcb131a6481e\n24333f8a63b6825ea9c5514f83c2829b004d1fee\n' > "$BUNDLE_DIR/licenses/android-sdk-license"
printf '84831b9409646a918e30573bab4c9c91346d8abd\n' > "$BUNDLE_DIR/licenses/android-sdk-preview-license"
printf '859f317696f67ef3d7f30a50a5560e7834b43903\n' > "$BUNDLE_DIR/licenses/android-sdk-arm-dbt-license"
printf '601085b94e77fbb98d06e26c2ef1c47a2b9b76e5\n' > "$BUNDLE_DIR/licenses/android-googletv-license"
printf '79120722343a6f314e0719f863036c702b0e6b2a\n' > "$BUNDLE_DIR/licenses/android-sdk-preview-license-old"
printf '33b6a2b64607f11b759f320ef9dff4ae5c47d97a\n' > "$BUNDLE_DIR/licenses/google-gdk-license"
printf 'e9acab5b5fbb560a72cfaecce8946896ff6aab9d\n' > "$BUNDLE_DIR/licenses/mips-android-sysimage-license"
printf 'd975f751698a77b662f1254ddbeed3901e976f5a\n' > "$BUNDLE_DIR/licenses/intel-android-extra-license"

# ---- 5. 用本地/宿主 sdkmanager 下载 Windows 镜像与组件 ----
echo "==> [5/5] 安装 Windows emulator, build-tools..."
# 如果宿主有 sdkmanager 则用宿主的 sdkmanager 下载指定平台镜像
SDKMGR=""
if [ -x "$HOME/Library/Caches/umeng-dau-client/sdk/cmdline-tools/latest/bin/sdkmanager" ]; then
  SDKMGR="$HOME/Library/Caches/umeng-dau-client/sdk/cmdline-tools/latest/bin/sdkmanager"
  export JAVA_HOME="$HOME/Library/Caches/umeng-dau-client/sdk/jre/Contents/Home"
fi

if [ -n "$SDKMGR" ]; then
  "$SDKMGR" --sdk_root="$BUNDLE_DIR" "emulator" "build-tools;34.0.0"
else
  echo "提示: 未检测到宿主 sdkmanager，请在 Windows 机器上直接运行 prepare-bundle.ps1"
fi

echo ""
echo "========================================"
echo "  Windows Bundle 准备完成"
echo "========================================"
du -sh "$BUNDLE_DIR"
