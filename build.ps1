# 构建脚本：加载 VS 编译环境（让链接器找到 Windows SDK），编译后把模型和桌面控制组件 agent-desktop.exe 复制到 exe 旁边。
# 用法：  .\build.ps1            # 调试版
#        .\build.ps1 -Release   # 发布版，并把能直接运行的整套文件放到 dist\（双击 dist\aeri.exe 即可）
#        .\build.ps1 -Android   # 调试版也顺带编译手机 App（发布版总会编译）
param([switch]$Release, [switch]$Run, [switch]$Test, [switch]$Android)

$ErrorActionPreference = 'Stop'
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = "$env:USERPROFILE\.cargo-target\va" }  # 短路径，避开 260 字符限制

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
    if ($_ -match '^(PATH|LIB|LIBPATH|INCLUDE|WindowsSdkDir|WindowsSDKVersion|VCToolsInstallDir)=(.*)$') {
        Set-Item -Path "env:$($Matches[1])" -Value $Matches[2]
    }
}
# 部分电脑上 SDK 注册信息不全，vcvars 只加了 ucrt，没加 um（kernel32.lib 在这里），手动补上
$kits = "${env:ProgramFiles(x86)}\Windows Kits\10"
$sdk = Get-ChildItem "$kits\Lib" -Directory | Where-Object { Test-Path "$($_.FullName)\um\x64\kernel32.lib" } |
    Sort-Object Name -Descending | Select-Object -First 1
if ($sdk -and $env:LIB -notmatch [regex]::Escape("$($sdk.Name)\um\x64")) {
    $env:LIB = "$env:LIB;$($sdk.FullName)\um\x64;$($sdk.FullName)\ucrt\x64"
    $inc = "$kits\Include\$($sdk.Name)"
    $env:INCLUDE = "$env:INCLUDE;$inc\um;$inc\shared;$inc\ucrt;$inc\winrt"
    $env:PATH = "$env:PATH;$kits\bin\$($sdk.Name)\x64"
}

# 桌面控制组件：固定版本，第一次编译时从 GitHub 下载并校验，之后用 vendor\ 里的缓存
$agentDesktopVersion = 'v1.0.2'
$agentDesktopSha256 = '2186c76e7df197c0e67365e45d36d869f74714c56bb8a73817e2a66f3e18e222'
function Get-AgentDesktop {
    $dir = Join-Path $PSScriptRoot "vendor\agent-desktop\$agentDesktopVersion"
    $exe = Join-Path $dir 'agent-desktop.exe'
    if (Test-Path -LiteralPath $exe) { return $exe }
    New-Item -ItemType Directory -Force $dir | Out-Null
    $name = "agent-desktop-$agentDesktopVersion-x86_64-pc-windows-msvc.tar.gz"
    $archive = Join-Path $dir $name
    Write-Host "下载桌面控制组件 agent-desktop $agentDesktopVersion ..."
    Invoke-WebRequest "https://github.com/vercel-labs/agent-desktop/releases/download/$agentDesktopVersion/$name" -OutFile $archive -UseBasicParsing
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $agentDesktopSha256) {
        Remove-Item -LiteralPath $archive
        throw 'agent-desktop 下载文件校验失败'
    }
    tar -xzf $archive -C $dir agent-desktop.exe
    if ($LASTEXITCODE -ne 0) { throw '解压 agent-desktop 失败' }
    Remove-Item -LiteralPath $archive
    return $exe
}

# 手机 App：版本号和电脑版一致（取 Cargo.toml），编译后连同版本信息放到 $dest\（exe 旁边的 android\），
# 电脑会把它传给配对过的手机更新。没有安卓 SDK 或 JDK 时跳过，不影响电脑版
function Build-AndroidApp([string]$dest) {
    $androidDir = Join-Path $PSScriptRoot 'android'
    $hasSdk = $env:ANDROID_HOME -or $env:ANDROID_SDK_ROOT -or (Test-Path (Join-Path $androidDir 'local.properties'))
    if (-not $hasSdk -or -not (Get-Command java -ErrorAction SilentlyContinue)) {
        Write-Warning '没有找到安卓 SDK 或 JDK，跳过手机 App（手机上就收不到 App 更新）'
        return
    }
    Push-Location $androidDir
    try {
        & .\gradlew.bat assembleRelease -q --console=plain
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($code -ne 0) { throw '编译手机 App 失败' }
    $apk = Join-Path $androidDir 'app\build\outputs\apk\release\app-release.apk'
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item -LiteralPath $apk -Destination (Join-Path $dest 'Aeri.apk') -Force
    $version = (Select-String -Path (Join-Path $PSScriptRoot 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $parts = @($version.Split('.') | ForEach-Object { [int]($_ -replace '\D.*$', '') })
    $info = [ordered]@{
        version_code = $parts[0] * 10000 + $parts[1] * 100 + $parts[2]  # 和 android\app\build.gradle.kts 的算法一致
        version_name = $version
        size = (Get-Item -LiteralPath $apk).Length
        sha256 = (Get-FileHash -LiteralPath $apk -Algorithm SHA256).Hash.ToLower()
    }
    # 不带 BOM 的 UTF-8，电脑端直接按 JSON 读
    [IO.File]::WriteAllText((Join-Path $dest 'Aeri.json'), ($info | ConvertTo-Json))
    Write-Host "手机 App v$version 已放到 $dest"
}

Push-Location $PSScriptRoot
try {
    $agentDesktop = Get-AgentDesktop
    [string[]]$profileArgs = @()
    if ($Release) { $profileArgs += '--release' }
    if ($Test) {
        cargo test @profileArgs
        exit $LASTEXITCODE
    }
    cargo build @profileArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $outDir = Join-Path $env:CARGO_TARGET_DIR ($(if ($Release) { 'release' } else { 'debug' }))
    Copy-Item -LiteralPath $agentDesktop -Destination $outDir -Force
    # Copy runtime models, tokens and the English wake-word lexicon; omit unused float/chunk-8 models.
    $modelsOut = Join-Path $outDir 'models'
    if (Test-Path $modelsOut) { Remove-Item -Recurse -Force $modelsOut }
    robocopy models $modelsOut /E /NFL /NDL /NJH /NJS /XD test_wavs `
        /XF *chunk-8* encoder-epoch-13-avg-2-chunk-16-left-64.onnx joiner-epoch-13-avg-2-chunk-16-left-64.onnx | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "复制模型失败（robocopy $LASTEXITCODE）" }
    $global:LASTEXITCODE = 0  # robocopy 用 1 表示“已复制”，不是错误
    if ($Release -or $Android) { Build-AndroidApp (Join-Path $outDir 'android') }
    $exe = Join-Path $outDir 'aeri.exe'
    if ($Release) {
        # 发布版放到固定的 dist\：开机自启登记的就是这里的路径，以后重新编译也不会变
        $dist = Join-Path $PSScriptRoot 'dist'
        $running = Get-Process aeri, voa, voice-assistant -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dist\*" }
        if ($running) {
            $running | Stop-Process -Force
            Start-Sleep -Milliseconds 500
            Write-Host "已关闭正在运行的旧版本"
        }
        robocopy $outDir $dist aeri.exe agent-desktop.exe *.dll /NFL /NDL /NJH /NJS | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "复制到 dist 失败（robocopy $LASTEXITCODE）" }
        'voice-assistant.exe', 'voa.exe' | ForEach-Object { Remove-Item -LiteralPath (Join-Path $dist $_) -ErrorAction SilentlyContinue }  # 改名前的旧程序
        robocopy $modelsOut (Join-Path $dist 'models') /MIR /NFL /NDL /NJH /NJS | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "复制模型到 dist 失败（robocopy $LASTEXITCODE）" }
        $androidOut = Join-Path $outDir 'android'
        if (Test-Path $androidOut) {
            robocopy $androidOut (Join-Path $dist 'android') /MIR /NFL /NDL /NJH /NJS | Out-Null
            if ($LASTEXITCODE -ge 8) { throw "复制手机 App 到 dist 失败（robocopy $LASTEXITCODE）" }
        }
        $global:LASTEXITCODE = 0
        $exe = Join-Path $dist 'aeri.exe'
        # Once installed, every release build also updates the installed application.
        $installedMarker = Join-Path $env:LOCALAPPDATA 'Programs\VoiceAssistant\installation.json'
        if (Test-Path -LiteralPath $installedMarker) {
            & (Join-Path $PSScriptRoot 'install.ps1') -Source $dist -Launch
            $exe = Join-Path $env:LOCALAPPDATA 'Programs\VoiceAssistant\aeri.exe'
            $Run = $false
        }
    }
    Write-Host "已生成：$exe"
    if ($Run) { & $exe }
} finally {
    Pop-Location
}
