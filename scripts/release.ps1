# 发版：编译发布版 → 打包 zip → 生成 SHA-256 → 推送标签 → 在 GitHub 创建 Release 并上传。
# 用法：.\scripts\release.ps1 -Notes 说明.md          （版本号取 Cargo.toml）
# 自动更新靠 Release 里的 Aeri-vX.Y.Z-windows-x64.zip 和它的校验值，文件名不要改。
param([Parameter(Mandatory)][string]$Notes, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$tag = "v$version"
if (git -C $root status --porcelain) { throw '还有没提交的改动，先提交再发版。' }
if (-not $SkipBuild) { & (Join-Path $root 'build.ps1') -Release }
$dist = Join-Path $root 'dist'
$notices = Join-Path $root 'assets\THIRD-PARTY-NOTICES.txt'
$out = Join-Path $env:TEMP "aeri-release-$version"
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path "$out\pkg" | Out-Null
Copy-Item "$dist\*" "$out\pkg" -Recurse
Copy-Item $notices "$out\pkg"
$zipName = "Aeri-$tag-windows-x64.zip"
$zip = Join-Path $out $zipName
# Windows 10 起自带的 tar 打 zip，路径分隔符是 /，各种解压工具都认
tar.exe -a -c -f $zip -C "$out\pkg" *
if ($LASTEXITCODE) { throw '打包失败' }
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $zipName" | Set-Content -Encoding ascii "$zip.sha256"
git -C $root tag -a $tag -m "Aeri $tag"
git -C $root push origin HEAD:main $tag
gh release create $tag $zip "$zip.sha256" -R yuqtai/Aeri --title "Aeri $tag" --notes-file $Notes
if ($LASTEXITCODE) { throw '创建 Release 失败（tag 已推送，修好后手动运行 gh release create）' }
"已发布 $tag：$zipName（SHA-256 $hash）"
