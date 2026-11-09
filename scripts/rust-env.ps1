# Локальные инструменты необязательны: в обычной установке используется cargo из PATH.
$localTools = Join-Path $PSScriptRoot '..\.tools'
if (Test-Path (Join-Path $localTools 'cargo\bin\cargo.exe')) {
    $env:CARGO_HOME = [IO.Path]::GetFullPath((Join-Path $localTools 'cargo'))
    $env:RUSTUP_HOME = [IO.Path]::GetFullPath((Join-Path $localTools 'rustup'))
    $env:PATH = (Join-Path $localTools 'cargo\bin') + ';' + $env:PATH
}
if (Test-Path (Join-Path $localTools 'w64devkit\bin')) {
    $env:PATH = (Join-Path $localTools 'w64devkit\bin') + ';' + $env:PATH
}
if (!(Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Для сборки нужен Rust. Для запуска скачайте готовый exe из Releases.' }
