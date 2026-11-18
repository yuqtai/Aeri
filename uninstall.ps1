# Uninstall Aeri: choose whether to keep or remove personal settings and API keys; verify binaries are unlocked before removing the registry entry.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms

$uiLanguage = if ($env:AERI_LANG) { $env:AERI_LANG } else { [Globalization.CultureInfo]::CurrentUICulture.Name }
$isChinese = $uiLanguage.Trim().StartsWith('zh', [StringComparison]::OrdinalIgnoreCase)
function Get-UiText([string]$English, [string]$Chinese) {
    if ($isChinese) { return $Chinese }
    return $English
}

$choice = [Windows.Forms.MessageBox]::Show(
    (Get-UiText `
        "Uninstall Aeri?`r`n`r`n• Click [Yes]: Uninstall Aeri only (keep personal settings and API keys)`r`n• Click [No]: Uninstall Aeri and remove all personal data`r`n• Click [Cancel]: Cancel uninstall" `
        "确定要卸载 Aeri 吗？`r`n`r`n• 点击【是】：仅卸载程序（保留个人设置和 API Key）`r`n• 点击【否】：连同个人配置数据一起清除`r`n• 点击【取消】：取消卸载"),
    (Get-UiText 'Uninstall Aeri' '卸载 Aeri'),
    [Windows.Forms.MessageBoxButtons]::YesNoCancel,
    [Windows.Forms.MessageBoxIcon]::Question
)
if ($choice -eq [Windows.Forms.DialogResult]::Cancel -or $choice -eq 'Cancel') { exit 0 }
$purgeData = ($choice -eq [Windows.Forms.DialogResult]::No -or $choice -eq 'No')

$expected = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs\VoiceAssistant'))
$actual = [IO.Path]::GetFullPath($PSScriptRoot)
$marker = Join-Path $actual 'installation.json'
if ($actual -ne $expected -or !(Test-Path -LiteralPath $marker)) {
    [Windows.Forms.MessageBox]::Show(
        (Get-UiText 'Installation directory verification failed. Uninstall aborted.' '安装目录校验失败，已停止卸载。'),
        'Aeri',
        [Windows.Forms.MessageBoxButtons]::OK,
        [Windows.Forms.MessageBoxIcon]::Error
    ) | Out-Null
    exit 1
}

$exe = Join-Path $actual 'aeri.exe'
$agentExe = Join-Path $actual 'agent-desktop.exe'
Get-Process aeri, agent-desktop -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -in @($exe, $agentExe) } |
    ForEach-Object {
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
        $_.WaitForExit(10000) | Out-Null
    }

# Delete program files except installation.json and uninstall.ps1 first: if any binary is locked, keep the marker and registry entry so the user can retry cleanly.
try {
    Get-ChildItem -LiteralPath $actual -Force |
        Where-Object { $_.Name -notin @('installation.json', 'uninstall.ps1') } |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
} catch {
    [Windows.Forms.MessageBox]::Show(
        (Get-UiText `
            "Some program files are currently in use and cannot be removed:`r`n$($_.Exception.Message)`r`n`r`nClose any related programs and try again." `
            "部分程序文件正在被占用，暂时无法卸载：`r`n$($_.Exception.Message)`r`n`r`n请关闭相关程序后再试。"),
        'Aeri',
        [Windows.Forms.MessageBoxButtons]::OK,
        [Windows.Forms.MessageBoxIcon]::Warning
    ) | Out-Null
    exit 1
}

$shell = New-Object -ComObject WScript.Shell
foreach ($folder in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {
    $link = Join-Path $folder 'Aeri.lnk'
    if ((Test-Path -LiteralPath $link) -and $shell.CreateShortcut($link).TargetPath -eq $exe) {
        Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue
    }
}

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startup = Get-ItemProperty -Path $runKey -Name VoiceAssistant -ErrorAction SilentlyContinue
if ($startup -and $startup.VoiceAssistant -eq ('"' + $exe + '" --autostart')) {
    Remove-ItemProperty -Path $runKey -Name VoiceAssistant -ErrorAction SilentlyContinue
}

if ($purgeData) {
    $appDataDir = Join-Path $env:APPDATA 'VoiceAssistant'
    if (Test-Path -LiteralPath $appDataDir) {
        Remove-Item -LiteralPath $appDataDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Remove-Item -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\VoiceAssistant' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue

# Clean up the remaining uninstall.ps1 and empty installation directory asynchronously from TEMP so the folder is not held open.
Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', "ping 127.0.0.1 -n 3 >nul & rmdir /s /q `"$actual`"" -WorkingDirectory $env:TEMP -WindowStyle Hidden
