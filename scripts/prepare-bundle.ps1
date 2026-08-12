# prepare-bundle.ps1 - Pre-download Android SDK tools to src-tauri\sdk-bundle\
# Windows version. Bundled into the installer for out-of-the-box offline use.
# Usage: From project root: powershell -ExecutionPolicy Bypass -File scripts\prepare-bundle.ps1
# Proxy: $env:HTTPS_PROXY="http://127.0.0.1:7897"
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
$BundleDir = Join-Path $ProjectRoot "src-tauri\sdk-bundle"
$Tmp = Join-Path $env:TEMP "umeng-bundle-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null

$Arch = $env:PROCESSOR_ARCHITECTURE
if ($Arch -eq "ARM64") {
    $JreArch = "aarch64"
    $SysImage = "system-images;android-34;google_apis;arm64-v8a"
} else {
    $JreArch = "x64"
    $SysImage = "system-images;android-34;google_apis;x86_64"
}

Write-Host "==> Arch: $Arch  JRE arch: $JreArch  Image: $SysImage"
Write-Host "==> Bundle directory: $BundleDir"
Write-Host ""

# Clean up old bundle
if (Test-Path $BundleDir) { Remove-Item -Recurse -Force $BundleDir }
New-Item -ItemType Directory -Force -Path $BundleDir | Out-Null
New-Item -ItemType File -Force -Path (Join-Path $BundleDir ".gitkeep") | Out-Null

# ---- 1. cmdline-tools ----
Write-Host "==> [1/5] Downloading cmdline-tools..."
Invoke-WebRequest -Uri "https://dl.google.com/android/repository/commandlinetools-win-11076708_latest.zip" -OutFile "$Tmp\cmdline.zip"
Expand-Archive -Path "$Tmp\cmdline.zip" -DestinationPath "$Tmp\cmdline-extract" -Force
New-Item -ItemType Directory -Force -Path "$BundleDir\cmdline-tools" | Out-Null
Move-Item "$Tmp\cmdline-extract\cmdline-tools" "$BundleDir\cmdline-tools\latest"

# ---- 2. JRE ----
Write-Host "==> [2/5] Downloading JRE (Temurin 17 $JreArch)..."
Invoke-WebRequest -Uri "https://api.adoptium.net/v3/binary/latest/17/ga/windows/$JreArch/jre/hotspot/normal/eclipse" -OutFile "$Tmp\jre.zip"
Expand-Archive -Path "$Tmp\jre.zip" -DestinationPath "$Tmp\jre-extract" -Force
$jreDir = Get-ChildItem "$Tmp\jre-extract" -Directory | Select-Object -First 1
Move-Item $jreDir.FullName "$BundleDir\jre"

# ---- 3. platform-tools ----
Write-Host "==> [3/5] Downloading platform-tools..."
Invoke-WebRequest -Uri "https://dl.google.com/android/repository/platform-tools-latest-windows.zip" -OutFile "$Tmp\platform-tools.zip"
Expand-Archive -Path "$Tmp\platform-tools.zip" -DestinationPath $BundleDir -Force

# ---- 4. write license files ----
Write-Host "==> [4/5] Writing license files..."
$LicDir = Join-Path $BundleDir "licenses"
New-Item -ItemType Directory -Force -Path $LicDir | Out-Null
Set-Content -Path "$LicDir\android-sdk-license" -Value "8933bad161af4178b1185d1a37fbf41ea5269c55`nd56f5187479451eabf01fb78af6dfcb131a6481e`n24333f8a63b6825ea9c5514f83c2829b004d1fee" -NoNewline
Set-Content -Path "$LicDir\android-sdk-preview-license" -Value "84831b9409646a918e30573bab4c9c91346d8abd" -NoNewline
Set-Content -Path "$LicDir\android-sdk-arm-dbt-license" -Value "859f317696f67ef3d7f30a50a5560e7834b43903" -NoNewline
Set-Content -Path "$LicDir\android-googletv-license" -Value "601085b94e77fbb98d06e26c2ef1c47a2b9b76e5" -NoNewline
Set-Content -Path "$LicDir\android-sdk-preview-license-old" -Value "79120722343a6f314e0719f863036c702b0e6b2a" -NoNewline
Set-Content -Path "$LicDir\google-gdk-license" -Value "33b6a2b64607f11b759f320ef9dff4ae5c47d97a" -NoNewline
Set-Content -Path "$LicDir\mips-android-sysimage-license" -Value "e9acab5b5fbb560a72cfaecce8946896ff6aab9d" -NoNewline
Set-Content -Path "$LicDir\intel-android-extra-license" -Value "d975f751698a77b662f1254ddbeed3901e976f5a" -NoNewline

# ---- 5. emulator + build-tools ----
Write-Host "==> [5/5] Installing emulator + build-tools (via sdkmanager)..."
$env:JAVA_HOME = "$BundleDir\jre"
$Sdkmgr = "$BundleDir\cmdline-tools\latest\bin\sdkmanager.bat"
& $Sdkmgr --sdk_root="$BundleDir" "emulator" "build-tools;34.0.0"

# Cleanup
Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue

# Summary
Write-Host ""
Write-Host "========================================"
Write-Host "  Bundle preparation complete!"
Write-Host "========================================"
$Size = (Get-ChildItem $BundleDir -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("  Size: {0:N0} MB" -f $Size)
Write-Host ""
Write-Host "Next step: cargo tauri build"
