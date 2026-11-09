param([switch]$Install)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
. (Join-Path $PSScriptRoot 'scripts\rust-env.ps1')
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Ошибка сборки Rust' }
$destination = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Path $destination -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\ClaudeWindowsStatusBar.exe') -Destination $destination -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'locales') -Destination $destination -Recurse -Force
if ($Install) {
    $exe = Join-Path $destination 'ClaudeWindowsStatusBar.exe'
    $installer = Start-Process -FilePath $exe -ArgumentList '--install' -WindowStyle Hidden -Wait -PassThru
    if ($installer.ExitCode -ne 0) { throw 'Ошибка установки интеграции Claude Code' }
    Start-Process -FilePath $exe -WindowStyle Hidden
}
