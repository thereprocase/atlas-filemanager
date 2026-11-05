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
Source: https://github.com/thereprocase/atlas-filemanager/tree/build/windows-full-demo
'@ | Set-Content "$payload/README.txt" -Encoding utf8

$probe = Start-Process -FilePath "$payload/atlas.exe" -ArgumentList '--version' -Wait -PassThru -NoNewWindow -RedirectStandardOutput 'target/full-demo/version.txt'
if ($probe.ExitCode -ne 0) { throw "Executable smoke test failed: $($probe.ExitCode)" }
$version = Get-Content 'target/full-demo/version.txt' -Raw
if ($version -notmatch '0\.1\.0-full-demo') { throw "Unexpected version: $version" }

$iscc = "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe"
if (-not (Test-Path $iscc)) {
    choco install innosetup --no-progress -y
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup installation failed.' }
}
& $iscc '/DAppVersion=0.1.0-full-demo' 'packaging/windows/atlas.iss'
if ($LASTEXITCODE -ne 0) { throw 'Installer packaging failed.' }
Copy-Item 'target/installer/AtlasSetup-0.1.0-full-demo.exe' $dist

$provenance = [ordered]@{
    repository = $env:GITHUB_REPOSITORY
    commit = $env:GITHUB_SHA
    run = "$env:GITHUB_SERVER_URL/$env:GITHUB_REPOSITORY/actions/runs/$env:GITHUB_RUN_ID"
    edition = 'Full (default features; free feature not enabled)'
    compiler = (& rustc --version)
    executableSmokeTest = $version.Trim()
}
$provenance | ConvertTo-Json | Set-Content "$payload/BUILD.json" -Encoding utf8
Compress-Archive -Path $payload -DestinationPath "$dist/Atlas-Full-Windows-x64.zip" -Force
Copy-Item "$payload/BUILD.json" $dist
Get-ChildItem $dist -File | Sort-Object Name | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    "$hash  $($_.Name)"
} | Set-Content "$dist/SHA256SUMS.txt" -Encoding ascii
