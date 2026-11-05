$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
if ($env:ATLAS_RUN -notmatch '^\d+$') { throw 'Invalid Atlas run ID.' }

$deadline = (Get-Date).AddMinutes(45)
do {
    $run = gh run view $env:ATLAS_RUN --repo $env:GITHUB_REPOSITORY --json status,conclusion,headSha | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Atlas build status.' }
    if ($run.status -eq 'completed') { break }
    Write-Host 'Waiting for the Atlas Windows compilation to finish.'
    Start-Sleep -Seconds 30
} while ((Get-Date) -lt $deadline)
if ($run.conclusion -ne 'success') { throw "Atlas build did not succeed: $($run.conclusion)" }

gh run download $env:ATLAS_RUN --repo $env:GITHUB_REPOSITORY --name Atlas-Full-Windows-x64 --dir target/atlas-demo
if ($LASTEXITCODE -ne 0) { throw 'Atlas artifact download failed.' }
foreach ($line in Get-Content target/atlas-demo/SHA256SUMS.txt) {
    $expected, $name = $line -split '  ', 2
    $actual = (Get-FileHash "target/atlas-demo/$name" -Algorithm SHA256).Hash
    if ($actual -ne $expected) { throw "Atlas checksum mismatch: $name" }
}
Expand-Archive target/atlas-demo/Atlas-Full-Windows-x64.zip target/combined
$payload = 'target/combined/Atlas-Full'
$robo = "$payload/RoboRightClick"
New-Item -ItemType Directory -Force $robo, dist | Out-Null
Copy-Item companion-src/artifacts/publish/* $robo -Recurse
Copy-Item companion-src/LICENSE "$robo/LICENSE.txt"
Copy-Item companion-src/README.md $robo

# Help exercises the Windows executable without installing or starting the tray.
$probe = Start-Process "$robo/RoboRightClick.exe" -ArgumentList '--help' -Wait -PassThru -NoNewWindow -RedirectStandardOutput target/robo-help.txt -RedirectStandardError target/robo-help-error.txt
if ($probe.ExitCode -ne 0) { throw "RoboRightClick executable check failed: $($probe.ExitCode)" }
if ((Get-Content target/robo-help.txt -Raw) -notmatch 'RoboRightClick copy') { throw 'RoboRightClick help output missing.' }

@'

RoboRightClick companion
------------------------
To enable Robo-Copy, Robo-Cut and Robo-Paste, open the RoboRightClick folder and
double-click RoboRightClick.exe. Accept its per-user install, then reopen Atlas.
Installation is optional; this ZIP does not install it automatically.

Atlas loads classic Windows shell commands into its right-click menu. Robo's
registered commands are expected there after installation; this interaction has
not yet been verified in a live Atlas window. Robo's own tray handles transfers,
conflict prompts, pause/cancel, and logging settings.

Atlas's normal Ctrl+C/Ctrl+V behavior remains unchanged. Robo's Ctrl+Shift+V
hotkey currently targets Explorer/desktop, not Atlas. Use Robo-Paste in the
right-click menu for Robo transfers. Atlas Ctrl+Z does not undo Robo transfers.

The separate Atlas installer contains Atlas only. The RoboRightClick companion
is in this ZIP. Both applications are unsigned personal builds.
'@ | Add-Content "$payload/README.txt" -Encoding utf8

$provenance = [ordered]@{
    atlas = (Get-Content "$payload/BUILD.json" -Raw | ConvertFrom-Json)
    roboRepository = 'https://github.com/thereprocase/robo-rightclick'
    roboCommit = $env:ROBO_COMMIT
    roboSdk = '10.0.111'
    roboSmokeTest = '--help exited 0 on Windows runner'
    shellMenuIntegration = 'Source-compatible Windows shell registration; Atlas UI interaction not yet tested'
    bundleCommit = $env:GITHUB_SHA
    bundleRun = "$env:GITHUB_SERVER_URL/$env:GITHUB_REPOSITORY/actions/runs/$env:GITHUB_RUN_ID"
}
$provenance | ConvertTo-Json -Depth 5 | Set-Content dist/BUILD.json -Encoding utf8
Copy-Item dist/BUILD.json "$payload/BUNDLE.json"
Compress-Archive $payload dist/Atlas-Full-with-RoboRightClick-Windows-x64.zip
Copy-Item target/atlas-demo/AtlasSetup-0.1.0-full-demo.exe dist
Get-ChildItem dist -File | Sort-Object Name | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
} | Set-Content dist/SHA256SUMS.txt -Encoding ascii
