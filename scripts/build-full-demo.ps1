[CmdletBinding()]
param([string]$Version = '0.1.0-full-demo', [string]$ArchiveName = 'Atlas-Full-Windows-x64.zip', [string]$RoboDirectory = '')
# Package the Full source build for personal evaluation. No Free feature is enabled.
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

# Reuse upstream's pinned, SHA-256-verified rclone payload.
& ./scripts/build-cloud-extension.ps1 -SkipBuild

$payload = Join-Path $repo 'target/full-demo/Atlas-Full'
$dist = Join-Path $repo 'dist'
New-Item -ItemType Directory -Force $payload, $dist | Out-Null
foreach ($exe in @('atlas.exe', 'atlas-search-service.exe', 'atlas-cloud-extension.exe', 'atlas-icon-extension.exe')) {
    Copy-Item "target/release/$exe" $payload
}
Copy-Item 'target/extensions/cloud/rclone.exe' $payload
Copy-Item 'crates/atlas-core/vendor/unrar-bin/UnRAR.exe' $payload
Copy-Item 'legal/THIRD-PARTY-NOTICES.txt' $payload
Copy-Item 'extensions/cloud/THIRD-PARTY-NOTICES.txt' "$payload/CLOUD-THIRD-PARTY-NOTICES.txt"
Copy-Item 'LICENSE' "$payload/LICENSE.txt"

# This launcher leaves the optional indexing helper off for a browsing demo.
@'
@echo off
set "ATLAS_DISABLE_SEARCH_SERVICE=1"
start "" "%~dp0atlas.exe" %*
'@ | Set-Content "$payload/Start-Atlas.cmd" -Encoding ascii
@'
Atlas Full - personal Windows x64 build

Extract the whole ZIP, then double-click Start-Atlas.cmd.
All edition features are enabled; no premium key is required.
Start-Atlas.cmd disables automatic startup of the optional search helper.
Run atlas.exe directly to use the normal upstream search-helper behavior.
Settings are stored in the usual Atlas user profile, not inside this folder.
The installer is optional and uses upstream's installation defaults.
This build is unsigned. Optional conversion tools may require separate downloads.
See BUILD.json for the exact source commit and build run.
'@ | Set-Content "$payload/README.txt" -Encoding utf8

$probe = Start-Process -FilePath "$payload/atlas.exe" -ArgumentList '--version' -Wait -PassThru -NoNewWindow -RedirectStandardOutput 'target/full-demo/version.txt'
if ($probe.ExitCode -ne 0) { throw "Executable smoke test failed: $($probe.ExitCode)" }
$versionOutput = Get-Content 'target/full-demo/version.txt' -Raw
if ($versionOutput -notmatch [regex]::Escape($Version)) { throw "Unexpected version: $versionOutput" }

$iscc = "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe"
if (-not (Test-Path $iscc)) {
    choco install innosetup --no-progress -y
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup installation failed.' }
}
& $iscc "/DAppVersion=$Version" 'packaging/windows/atlas.iss'
if ($LASTEXITCODE -ne 0) { throw 'Installer packaging failed.' }
Copy-Item "target/installer/AtlasSetup-$Version.exe" $dist

if ($RoboDirectory) {
    $robo = Join-Path $payload 'RoboRightClick'
    New-Item -ItemType Directory -Force $robo | Out-Null
    Copy-Item "$RoboDirectory/artifacts/publish/*" $robo -Recurse
    Copy-Item "$RoboDirectory/LICENSE" "$robo/LICENSE.txt"
    Copy-Item "$RoboDirectory/README.md" $robo
    @'

RoboRightClick is included in the RoboRightClick folder. Double-click its EXE
and accept its per-user install to register Robo-Copy, Robo-Cut and Robo-Paste.
Atlas loads registered Windows shell commands in its right-click menu.
Atlas Ctrl+C/Ctrl+V continues to use Atlas. Robo's Explorer hotkey does not target
Atlas. Robo transfers are not part of Atlas undo. Installation is optional.
The separate Atlas installer contains Atlas only; the companion is in this ZIP.
'@ | Add-Content "$payload/README.txt" -Encoding utf8
}
$provenance = [ordered]@{
    repository = $env:GITHUB_REPOSITORY
    commit = $env:GITHUB_SHA
    run = "$env:GITHUB_SERVER_URL/$env:GITHUB_REPOSITORY/actions/runs/$env:GITHUB_RUN_ID"
    roboCommit = $env:ROBO_COMMIT
    edition = 'Full (default features; free feature not enabled)'
    compiler = (& rustc --version)
    executableSmokeTest = $versionOutput.Trim()
}
$provenance | ConvertTo-Json | Set-Content "$payload/BUILD.json" -Encoding utf8
Compress-Archive -Path $payload -DestinationPath "$dist/$ArchiveName" -Force
Copy-Item "$payload/BUILD.json" $dist
Get-ChildItem $dist -File | Sort-Object Name | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    "$hash  $($_.Name)"
} | Set-Content "$dist/SHA256SUMS.txt" -Encoding ascii
