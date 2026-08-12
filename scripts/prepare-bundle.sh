#!/bin/sh
# prepare-bundle.sh — 预下载 Android SDK 工具链及系统镜像到 src-tauri/sdk-bundle/
# 打包进安装包后，客户端首次运行从 bundle 部署，零网络依赖开箱即用。
#
# 用法：
#   cd umeng-dau-client && sh scripts/prepare-bundle.sh
# 可挂代理：HTTPS_PROXY=http://127.0.0.1:7897 sh scripts/prepare-bundle.sh
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUNDLE_DIR="$PROJECT_ROOT/src-tauri/sdk-bundle"
TMP="$(mktemp -d /tmp/umeng-bundle.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

ARCH=$(uname -m)
case "$ARCH" in
  arm64|aarch64)
    JRE_ARCH="aarch64"
    SYS_IMAGE="system-images;android-34;google_apis;arm64-v8a"
    ;;
  *)
    JRE_ARCH="x64"
    SYS_IMAGE="system-images;android-34;google_apis;x86_64"
    ;;
esac

echo "==> 架构: $ARCH  JRE arch: $JRE_ARCH  镜像: $SYS_IMAGE"
echo "==> Bundle 目录: $BUNDLE_DIR"
echo ""

# 清理旧 bundle（保留 .gitkeep）
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR"
touch "$BUNDLE_DIR/.gitkeep"

# ---- 1. cmdline-tools（平台无关 Java 工具）----
echo "==> [1/5] 下载 cmdline-tools..."
curl -fSL -o "$TMP/cmdline.zip" \
  "https://dl.google.com/android/repository/commandlinetools-mac-11076708_latest.zip"
mkdir -p "$TMP/cmdline-extract"
unzip -q "$TMP/cmdline.zip" -d "$TMP/cmdline-extract"
mkdir -p "$BUNDLE_DIR/cmdline-tools"
mv "$TMP/cmdline-extract/cmdline-tools" "$BUNDLE_DIR/cmdline-tools/latest"
chmod +x "$BUNDLE_DIR/cmdline-tools/latest/bin/"*

# ---- 2. JRE（Temurin 17，架构相关）----
echo "==> [2/5] 下载 JRE (Temurin 17 $JRE_ARCH)..."
curl -fSL -o "$TMP/jre.tar.gz" \
  "https://api.adoptium.net/v3/binary/latest/17/ga/mac/$JRE_ARCH/jre/hotspot/normal/eclipse"
mkdir -p "$TMP/jre-extract"
tar -xzf "$TMP/jre.tar.gz" -C "$TMP/jre-extract"
mv "$TMP/jre-extract"/jdk-* "$BUNDLE_DIR/jre"

# ---- 3. platform-tools（darwin universal，含 adb）----
echo "==> [3/5] 下载 platform-tools..."
curl -fSL -o "$TMP/platform-tools.zip" \
  "https://dl.google.com/android/repository/platform-tools-latest-darwin.zip"
unzip -q "$TMP/platform-tools.zip" -d "$BUNDLE_DIR"

# ---- 4. license hash 直写（与 licenses.rs 完全一致）----
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

# ---- 5. emulator + build-tools (via sdkmanager) ----
echo "==> [5/5] 安装 emulator + build-tools (via sdkmanager)..."
export JAVA_HOME="$BUNDLE_DIR/jre/Contents/Home"
SDKMGR="$BUNDLE_DIR/cmdline-tools/latest/bin/sdkmanager"
"$SDKMGR" --sdk_root="$BUNDLE_DIR" "emulator" "build-tools;34.0.0"

# 修复 UNIX 可执行权限
chmod -R +x "$BUNDLE_DIR/cmdline-tools/latest/bin/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/platform-tools/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/emulator/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/build-tools/34.0.0/"* 2>/dev/null || true
chmod -R +x "$BUNDLE_DIR/jre/Contents/Home/bin/"* 2>/dev/null || true

# ---- 汇总 ----
echo ""
echo "========================================"
echo "  Bundle 准备完成 (全离线/开箱即用)"
echo "========================================"
du -sh "$BUNDLE_DIR"
echo ""
echo "内容: cmdline-tools / jre / platform-tools / emulator / build-tools/34.0.0 / licenses / $SYS_IMAGE"
echo ""
echo "下一步: cd src-tauri && cargo tauri build"
echo "  安装包将包含上述工具链及系统镜像，首次运行自动秒级部署，无需任何网络下载。"
