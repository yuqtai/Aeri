# Uninstall application files; retain the user's configuration and API keys.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
if ([Windows.Forms.MessageBox]::Show('卸载 Aeri？你的设置和 API Key 将保留。', 'Aeri', 'YesNo', 'Question') -ne 'Yes') { exit }
$expected = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs\VoiceAssistant'))
$actual = [IO.Path]::GetFullPath($PSScriptRoot)
if ($actual -ne $expected -or !(Test-Path -LiteralPath (Join-Path $actual 'installation.json'))) { throw '安装目录校验失败，停止卸载。' }
$exe = Join-Path $actual 'aeri.exe'
Get-Process aeri -ErrorAction SilentlyContinue | Where-Object Path -eq $exe | ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(10000) | Out-Null }
$shell = New-Object -ComObject WScript.Shell
foreach ($folder in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {
    $link = Join-Path $folder 'Aeri.lnk'
    if ((Test-Path -LiteralPath $link) -and $shell.CreateShortcut($link).TargetPath -eq $exe) { Remove-Item -LiteralPath $link -Force }
}
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startup = Get-ItemProperty -Path $runKey -Name VoiceAssistant -ErrorAction SilentlyContinue
if ($startup -and $startup.VoiceAssistant -eq ('"' + $exe + '" --autostart')) { Remove-ItemProperty -Path $runKey -Name VoiceAssistant }
Remove-Item -LiteralPath $actual -Recurse -Force
Remove-Item -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\VoiceAssistant' -Recurse -Force
