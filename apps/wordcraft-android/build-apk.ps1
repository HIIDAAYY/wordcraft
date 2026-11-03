# Build WordCraft for Android as an installable APK (arm64) on Windows: the same steps as
# build-apk.sh, with the Android SDK's Windows tools (d8.bat, apksigner.bat, the NDK's .cmd
# compiler wrappers) and no Gradle.
#
# Needs: Rust with the aarch64-linux-android target, a JDK (17+, JAVA_HOME or on PATH), an Android
# SDK with build-tools, a platform (android-35 by default) and the NDK, and Git for Windows (its
# patch.exe). Point ANDROID_HOME at the SDK.
#
#   $env:ANDROID_HOME = "C:\Android\Sdk"; apps\wordcraft-android\build-apk.ps1
#
# The APK is signed with apps\wordcraft-android\keys\wordcraft-android.jks (gitignored), created
# on the first build. Keep it: Android installs an update only over a build signed with the same key.
$ErrorActionPreference = "Continue"
$Here = $PSScriptRoot
$Root = (Resolve-Path "$Here\..\..").Path

function Need($path, $what) { if (-not (Test-Path $path)) { throw "missing ${what}: $path" } }
# Tools report progress on stderr: not an error unless they exit with one (Windows PowerShell turns
# redirected stderr lines into errors, which "Stop" would throw on), so each call checks its exit code.
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" } }

$Sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { $env:ANDROID_SDK_ROOT }
if (-not $Sdk -or -not (Test-Path $Sdk)) { throw "set ANDROID_HOME to your Android SDK" }
$Ndk = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME } else { (Get-ChildItem "$Sdk\ndk" -Directory | Sort-Object Name | Select-Object -Last 1).FullName }
Need $Ndk "Android NDK (sdkmanager `"ndk;27.2.12479018`")"
$Platform = if ($env:ANDROID_PLATFORM) { $env:ANDROID_PLATFORM } else { "android-35" }
$AndroidJar = "$Sdk\platforms\$Platform\android.jar"
Need $AndroidJar "platform (sdkmanager `"platforms;$Platform`")"
$BuildTools = (Get-ChildItem "$Sdk\build-tools" -Directory | Sort-Object { [version]$_.Name } | Select-Object -Last 1).FullName
$Java = if ($env:JAVA_HOME) { "$env:JAVA_HOME\bin" } else { Split-Path (Get-Command javac).Source }
$Patch = (Get-Command patch -ErrorAction SilentlyContinue).Source
if (-not $Patch) { $Patch = "$env:ProgramFiles\Git\usr\bin\patch.exe" }
Need $Patch "patch.exe (Git for Windows)"

$MinSdk = 26
$TargetSdk = $Platform -replace '^android-', ''
$AppId = if ($env:APP_ID) { $env:APP_ID } else { "wordcraft.android" }
$Target = "aarch64-linux-android"
$Abi = "arm64-v8a"
$Version = (Select-String -Path "$Root\Cargo.toml" -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$v = $Version.Split('.')
$VersionCode = (([int]$v[0] * 10000 + [int]$v[1] * 100 + [int]$v[2]) * 100)

$Llvm = "$Ndk\toolchains\llvm\prebuilt\windows-x86_64\bin"
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "$Llvm\$Target$MinSdk-clang.cmd"
$env:CC_aarch64_linux_android = "$Llvm\$Target$MinSdk-clang.cmd"
$env:CXX_aarch64_linux_android = "$Llvm\$Target$MinSdk-clang++.cmd"
$env:AR_aarch64_linux_android = "$Llvm\llvm-ar.exe"
$env:RANLIB_aarch64_linux_android = "$Llvm\llvm-ranlib.exe"
# 16 KB pages (Android 15 devices) as well as 4 KB.
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS = "-C link-arg=-Wl,-z,max-page-size=16384"

$Out = "$Root\dist\android"
$Work = "$Root\target\android-apk"
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force "$Work\classes", "$Work\dex", "$Work\res", "$Work\apk\lib\$Abi", $Out | Out-Null

Write-Host "==> winit with Android modifier keys"
# winit 0.30 never reports Shift/Ctrl/Alt on Android; the patch is applied to a copy of the locked
# winit for this build only, and Cargo.lock is put back afterwards.
$WinitVersion = (Select-String -Path "$Root\Cargo.lock" -Pattern '^name = "winit"$' -Context 0, 1 | Select-Object -First 1).Context.PostContext[0] -replace 'version = |"', ''
Push-Location $Root; & cargo fetch --target $Target -q; Pop-Location; Check "cargo"
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$env:USERPROFILE\.cargo" }
$WinitSrc = (Get-ChildItem "$CargoHome\registry\src\*\winit-$WinitVersion" -Directory | Select-Object -First 1).FullName
$WinitPatched = "$Root\target\android-winit\winit-$WinitVersion"
if (Test-Path $WinitPatched) { Remove-Item -Recurse -Force $WinitPatched }
New-Item -ItemType Directory -Force (Split-Path $WinitPatched) | Out-Null
Copy-Item -Recurse $WinitSrc $WinitPatched
Get-Content -Raw "$Here\winit-android-modifiers.patch" | & $Patch -s -p1 -d $WinitPatched; Check "$Patch"
Copy-Item "$Root\Cargo.lock" "$Work\Cargo.lock.orig"

try {
    Write-Host "==> Fonts"
    # Office fonts are drawn with their metric-compatible open substitutes from craft-fonts
    # (Liberation Serif, Sans and Mono, Carlito, Caladea); the large CJK faces stay out (Android has its own).
    $Fonts = $env:CRAFT_FONTS_DIR
    if (-not $Fonts) {
        $Fonts = "$Root\target\craft-fonts"
        if (-not (Test-Path "$Fonts\fonts")) { & git clone -q --depth 1 https://github.com/storytold/craft-fonts $Fonts }; Check "git"
    }
    # Staged in place and rewritten only when it changes, so a rebuild doesn't recompile the fonts
    # crate and everything above it.
    $Stage = "$Root\target\android-craft-fonts"
    New-Item -ItemType Directory -Force "$Stage\fonts" | Out-Null
    foreach ($d in Get-ChildItem "$Fonts\fonts" -Directory) {
        if (-not (Test-Path "$Stage\fonts\$($d.Name)")) { New-Item -ItemType Junction -Path "$Stage\fonts\$($d.Name)" -Target $d.FullName | Out-Null }
    }
    $lines = @(Get-Content "$Fonts\fonts\latin-manifest.txt" | Where-Object { $_ -match '^(Carlito|Caladea|Liberation Sans|Liberation Serif|Liberation Mono) \|' })
    $lines += Get-Content "$Fonts\fonts\manifest.txt" | Where-Object { $_ -notmatch '^(#|Shippori Mincho|BIZ UD|Noto Sans CJK)' }
    if (-not ($lines | Where-Object { $_ -match '^Liberation Serif' })) { throw "$Fonts has no Liberation Serif (craft-fonts too old?)" }
    $manifest = "$Stage\fonts\manifest.txt"
    $text = ($lines -join "`n") + "`n"
    if (-not (Test-Path $manifest) -or [IO.File]::ReadAllText($manifest) -ne $text) { [IO.File]::WriteAllText($manifest, $text) }
    $env:CRAFT_FONTS_DIR = $Stage

    Write-Host "==> Rust (release, $Target)"
    $WinitToml = $WinitPatched -replace '\\', '/'
    Push-Location $Root
    & cargo build -p wordcraft-android --lib --release --target $Target --config "patch.crates-io.winit.path='$WinitToml'"; Check "cargo"
    Pop-Location
    $So = "$Work\apk\lib\$Abi\libwordcraft_android.so"
    Copy-Item "$Root\target\$Target\release\libwordcraft_android.so" $So
    & "$Llvm\llvm-strip.exe" --strip-unneeded $So; Check "llvm-strip.exe"
    if ((& "$Llvm\llvm-readelf.exe" -d $So) -match 'libc\+\+_shared.so') {
        Copy-Item "$Ndk\toolchains\llvm\prebuilt\windows-x86_64\sysroot\usr\lib\$Target\libc++_shared.so" "$Work\apk\lib\$Abi\"
    }

    Write-Host "==> Java"
    # Through argument files: Windows PowerShell mangles some arguments it passes to programs.
    $q = { param($x) '"' + ($x -replace '\\', '/') + '"' }
    $javacArgs = @("--release", "11", "-nowarn", "-classpath", (& $q $AndroidJar), "-d", (& $q "$Work\classes"))
    $javacArgs += Get-ChildItem -Recurse "$Here\android\java" -Filter *.java | ForEach-Object { & $q $_.FullName }
    [IO.File]::WriteAllLines("$Work\javac.args", [string[]]$javacArgs)
    & "$Java\javac.exe" "@$Work\javac.args"; Check "javac"
    & "$Java\jar.exe" cf "$Work\classes.jar" -C "$Work\classes" .; Check "jar"
    & "$BuildTools\d8.bat" --release --min-api $MinSdk --lib $AndroidJar --output "$Work\dex" "$Work\classes.jar"; Check "d8"

    Write-Host "==> Resources"
    $Icons = "$Root\assets\app-icon\hicolor"
    foreach ($pair in "mdpi:48", "hdpi:64", "xhdpi:128", "xxhdpi:256", "xxxhdpi:512") {
        $density, $size = $pair.Split(':')
        New-Item -ItemType Directory -Force "$Work\res\mipmap-$density" | Out-Null
        Copy-Item "$Icons\${size}x$size\apps\ai.storyteller.wordcraft.png" "$Work\res\mipmap-$density\ic_launcher.png"
    }
    & "$BuildTools\aapt2.exe" compile --dir "$Work\res" -o "$Work\res.zip"; Check "aapt2.exe"
    & "$BuildTools\aapt2.exe" link -o "$Work\unsigned.apk" -I $AndroidJar --manifest "$Here\android\AndroidManifest.xml" `
        --rename-manifest-package $AppId --min-sdk-version $MinSdk --target-sdk-version $TargetSdk `
        --version-code $VersionCode --version-name $Version "$Work\res.zip"; Check "aapt2 link"

    Write-Host "==> Package"
    Copy-Item "$Work\dex\classes.dex" "$Work\apk\"
    Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::Open("$Work\unsigned.apk", 'Update')
    try {
        foreach ($f in Get-ChildItem -Recurse -File "$Work\apk") {
            $name = $f.FullName.Substring("$Work\apk\".Length) -replace '\\', '/'
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $f.FullName, $name, 'Optimal') | Out-Null
        }
    } finally { $zip.Dispose() }
    & "$BuildTools\zipalign.exe" -f -P 16 4 "$Work\unsigned.apk" "$Work\aligned.apk"; Check "zipalign.exe"

    Write-Host "==> Sign"
    $Keystore = if ($env:KEYSTORE) { $env:KEYSTORE } else { "$Here\keys\wordcraft-android.jks" }
    $Pass = if ($env:KEYSTORE_PASS) { $env:KEYSTORE_PASS } else { "wordcraft" }
    $Alias = if ($env:KEY_ALIAS) { $env:KEY_ALIAS } else { "wordcraft" }
    if (-not (Test-Path $Keystore)) {
        New-Item -ItemType Directory -Force (Split-Path $Keystore) | Out-Null
        & "$Java\keytool.exe" -genkeypair -keystore $Keystore -storepass $Pass -keypass $Pass -alias $Alias -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=WordCraft for Android (local build)"; Check "keytool.exe"
        Write-Host "    created signing key $Keystore (keep it to install updates over this build)"
    }
    $Apk = "$Out\WordCraft-$Version-arm64.apk"
    & "$BuildTools\apksigner.bat" sign --ks $Keystore --ks-pass "pass:$Pass" --ks-key-alias $Alias --min-sdk-version $MinSdk --out $Apk "$Work\aligned.apk"; Check "apksigner.bat"
    & "$BuildTools\apksigner.bat" verify $Apk; Check "apksigner.bat"
    Write-Host ("==> $Apk ({0:N0} MB)" -f ((Get-Item $Apk).Length / 1MB))
} finally {
    Copy-Item "$Work\Cargo.lock.orig" "$Root\Cargo.lock" -Force
}
