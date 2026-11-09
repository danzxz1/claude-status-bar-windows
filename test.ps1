$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
. (Join-Path $PSScriptRoot 'scripts\rust-env.ps1')
cargo fmt -- --check
if ($LASTEXITCODE -ne 0) { throw 'Нарушено форматирование' }
cargo clippy --all-targets --locked -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'Статический анализ не пройден' }
cargo test --locked
if ($LASTEXITCODE -ne 0) { throw 'Тесты не пройдены' }
