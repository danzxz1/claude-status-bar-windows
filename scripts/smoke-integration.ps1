param([string]$Exe = (Join-Path $PSScriptRoot '..\target\release\ClaudeWindowsStatusBar.exe'))
$ErrorActionPreference='Stop'
$testRoot=Join-Path ([IO.Path]::GetTempPath()) ('cwsb-e2e-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
$settingsPath=Join-Path $testRoot 'settings.json'
$utf8=New-Object Text.UTF8Encoding($false)
$original=@{statusLine=@{type='command';command="printf 'исходная строка'";padding=2};hooks=@{Stop=@(@{hooks=@(@{type='command';command='echo foreign'})})};keepMe=$true}
[IO.File]::WriteAllText($settingsPath,($original|ConvertTo-Json -Depth 10),$utf8)
function Run-Native([string]$Arguments,[string]$InputData='') {
    $info=New-Object Diagnostics.ProcessStartInfo
    $info.FileName=[IO.Path]::GetFullPath($Exe);$info.Arguments=$Arguments
    $info.UseShellExecute=$false;$info.CreateNoWindow=$true
    $info.RedirectStandardInput=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    $info.StandardOutputEncoding=$utf8;$info.StandardErrorEncoding=$utf8
    $info.EnvironmentVariables['CWSB_DATA_DIR']=$testRoot
    $info.EnvironmentVariables['CWSB_SETTINGS_PATH']=$settingsPath
    $info.EnvironmentVariables['CWSB_TEST_MODE']='1'
    $process=New-Object Diagnostics.Process;$process.StartInfo=$info
    $process.Start()|Out-Null;$bytes=$utf8.GetBytes($InputData);$process.StandardInput.BaseStream.Write($bytes,0,$bytes.Length);$process.StandardInput.Close()
    $output=$process.StandardOutput.ReadToEnd();$errors=$process.StandardError.ReadToEnd()
    if(!$process.WaitForExit(10000)){$process.Kill();throw 'Превышен таймаут'}
    if($process.ExitCode -ne 0){throw ('Ошибка exe: '+$errors)}
    $process.Dispose();return $output
}
try {
    Run-Native '--install' | Out-Null
    Run-Native '--install' | Out-Null
    $settings=Get-Content $settingsPath -Raw -Encoding UTF8|ConvertFrom-Json
    if($settings.hooks.Stop.Count -ne 2){throw 'Повторная установка дублирует hooks'}
    if($settings.statusLine.padding -ne 2){throw 'Потеряны параметры statusLine'}
    $output=Run-Native '--statusline' '{"rate_limits":{"five_hour":{"used_percentage":33,"resets_at":2000000000}}}'
    if($output -cne 'исходная строка'){throw ('Исходный вывод изменён: '+$output)}
    $usage=Get-Content (Join-Path $testRoot 'usage.json') -Raw -Encoding UTF8|ConvertFrom-Json
    if($usage.FiveHour.Used -ne 33 -or $null -ne $usage.SevenDay){throw 'Неверные данные лимитов'}
    Run-Native '--hook UserPromptSubmit' '{"session_id":"../../test","cwd":"G:\\Проекты\\пример"}' | Out-Null
    $state=Get-ChildItem (Join-Path $testRoot 'sessions') -Filter '*.json'|Get-Content -Raw -Encoding UTF8|ConvertFrom-Json
    if($state.State -ne 'thinking' -or $state.Project -ne 'пример'){throw 'Неверное состояние hook'}
    Run-Native '--hook Stop' '{broken json' | Out-Null
    Run-Native '--uninstall' | Out-Null
    $restored=Get-Content $settingsPath -Raw -Encoding UTF8|ConvertFrom-Json
    if($restored.statusLine.command -cne $original.statusLine.command -or !$restored.keepMe -or $restored.hooks.Stop.Count -ne 1 -or $restored.hooks.Stop[0].hooks[0].command -ne 'echo foreign'){throw 'Настройки не восстановились'}
    'Проверка готового exe: hooks, statusLine, установка и удаление пройдены.'
} finally {
    $resolved=[IO.Path]::GetFullPath($testRoot)
    $tempBase=[IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if(!$resolved.StartsWith($tempBase,[StringComparison]::OrdinalIgnoreCase)){throw 'Некорректная временная папка'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
