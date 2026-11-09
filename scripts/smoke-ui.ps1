param([string]$Exe = (Join-Path $PSScriptRoot '..\target\release\ClaudeWindowsStatusBar.exe'), [switch]$SeparateIcons, [switch]$Vertical, [switch]$DisabledTracking, [switch]$ToggleTracking)
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class NativeTrayTest {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X,Y; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left,Top,Right,Bottom; }
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c,string t);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
        [StructLayout(LayoutKind.Sequential)] public struct ICONID { public uint Size; public IntPtr Window; public uint Id; public Guid Guid; }
    [DllImport("shell32.dll")] public static extern int Shell_NotifyIconGetRect(ref ICONID id,out RECT rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT rect);
    [DllImport("user32.dll")] public static extern IntPtr GetSubMenu(IntPtr menu,int position);
    [DllImport("user32.dll")] public static extern uint GetMenuState(IntPtr menu,uint item,uint flags);
    [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr menu);
    [DllImport("user32.dll")] public static extern bool GetMenuItemRect(IntPtr w,IntPtr menu,uint item,out RECT rect);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetMenuString(IntPtr menu,uint item,StringBuilder text,int cap,uint flags);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint dx,uint dy,uint data,UIntPtr extra);
}
"@
$originalPoint = New-Object NativeTrayTest+POINT
[NativeTrayTest]::GetCursorPos([ref]$originalPoint) | Out-Null
$previousData=$env:CWSB_DATA_DIR
$temporaryData=Join-Path ([IO.Path]::GetTempPath()) ('cwsb-ui-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporaryData -Force | Out-Null
$settings=@{SeparateIcons=[bool]$SeparateIcons;PanelLayout=$(if($Vertical){'Vertical'}else{'Horizontal'});PanelScale=100;TrackClaude=(!$DisabledTracking);TrackCodex=(!$DisabledTracking)}
$settings|ConvertTo-Json|Set-Content (Join-Path $temporaryData 'appearance.json') -Encoding UTF8
$env:CWSB_DATA_DIR=$temporaryData
$testProcess = Start-Process -FilePath $Exe -ArgumentList '--demo' -WindowStyle Hidden -PassThru
try {
    $window=[IntPtr]::Zero
    for($i=0;$i -lt 30;$i++) { Start-Sleep -Milliseconds 100; $window=[NativeTrayTest]::FindWindow('ClaudeWindowsStatusBarRust','Claude + Codex — Rust'); if($window -ne [IntPtr]::Zero){break} }
    if($window -eq [IntPtr]::Zero){throw 'Не найдено тестовое окно. Закройте другие экземпляры приложения.'}
    $panel=[NativeTrayTest]::FindWindow('ClaudeWindowsStatusBarRust','Claude + Codex — состояние и лимиты')
    if($SeparateIcons -and !$DisabledTracking){foreach($iconIndex in @(1,2)){$iconId=New-Object NativeTrayTest+ICONID;$iconId.Size=[Runtime.InteropServices.Marshal]::SizeOf($iconId);$iconId.Window=$window;$iconId.Id=$iconIndex;$iconRect=New-Object NativeTrayTest+RECT;if([NativeTrayTest]::Shell_NotifyIconGetRect([ref]$iconId,[ref]$iconRect) -lt 0){throw ('Не найден отдельный значок '+$iconIndex)}}}
    $layoutBounds=New-Object NativeTrayTest+RECT
    [NativeTrayTest]::GetWindowRect($panel,[ref]$layoutBounds) | Out-Null
    if($Vertical -and !$DisabledTracking -and ($layoutBounds.Bottom-$layoutBounds.Top) -le ($layoutBounds.Right-$layoutBounds.Left)){throw 'Вертикальная панель имеет неверные размеры'}
    [NativeTrayTest]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]1024) | Out-Null
    Start-Sleep -Milliseconds 200
    if([NativeTrayTest]::IsWindowVisible($panel)){throw 'ЛКМ не скрывает панель'}
    [NativeTrayTest]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]1024) | Out-Null
    Start-Sleep -Milliseconds 200
    if(![NativeTrayTest]::IsWindowVisible($panel)){throw 'ЛКМ не показывает панель'}
    $beforeBounds=New-Object NativeTrayTest+RECT
    $afterBounds=New-Object NativeTrayTest+RECT
    [NativeTrayTest]::GetWindowRect($panel,[ref]$beforeBounds) | Out-Null
    [NativeTrayTest]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x0200) | Out-Null
    Start-Sleep -Milliseconds 150
    [NativeTrayTest]::GetWindowRect($panel,[ref]$afterBounds) | Out-Null
    if($beforeBounds.Left -ne $afterBounds.Left -or $beforeBounds.Top -ne $afterBounds.Top){throw 'Панель перемещается при повторном событии мыши'}
    [NativeTrayTest]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x007B) | Out-Null
    Start-Sleep -Milliseconds 300
    $popup=[NativeTrayTest]::FindWindow('#32768',$null)
    if($popup -eq [IntPtr]::Zero){throw 'Меню не открылось'}
    $menu=[NativeTrayTest]::SendMessage($popup,0x01E1,[IntPtr]::Zero,[IntPtr]::Zero)
    $count=[NativeTrayTest]::GetMenuItemCount($menu)
    [NativeTrayTest]::PostMessage($popup,0x0100,[IntPtr]0x24,[IntPtr]::Zero)|Out-Null
    Start-Sleep -Milliseconds 100
    for($item=0;$item -lt $count;$item++){$entry=New-Object Text.StringBuilder 128;[NativeTrayTest]::GetMenuString($menu,$item,$entry,128,0x0400)|Out-Null;if($entry.ToString() -eq 'Перейти к задаче'){throw 'Удалённый механизм остался в меню'}}
    $trackingMenu=[NativeTrayTest]::GetSubMenu($menu,0)
    foreach($command in @(80,81)){$checked=([NativeTrayTest]::GetMenuState($trackingMenu,$command,0) -band 8) -ne 0;if($checked -eq [bool]$DisabledTracking){throw 'Флажки отслеживания не совпадают с сохранёнными настройками'}}
    if($DisabledTracking){$iconId=New-Object NativeTrayTest+ICONID;$iconId.Size=[Runtime.InteropServices.Marshal]::SizeOf($iconId);$iconId.Window=$window;$iconId.Id=1;$iconRect=New-Object NativeTrayTest+RECT;if([NativeTrayTest]::Shell_NotifyIconGetRect([ref]$iconId,[ref]$iconRect) -lt 0){throw 'При отключении обоих агентов пропал значок с меню'}}
    if($ToggleTracking){
        foreach($agentPosition in @(0,1)){
            $parentBounds=New-Object NativeTrayTest+RECT
            [NativeTrayTest]::GetMenuItemRect($window,$menu,0,[ref]$parentBounds)|Out-Null
            [NativeTrayTest]::SetCursorPos(($parentBounds.Left+30),(($parentBounds.Top+$parentBounds.Bottom)/2))|Out-Null
            [NativeTrayTest]::PostMessage($popup,0x0100,[IntPtr]0x24,[IntPtr]::Zero)|Out-Null
            Start-Sleep -Milliseconds 50
            [NativeTrayTest]::PostMessage($popup,0x0100,[IntPtr]0x27,[IntPtr]::Zero)|Out-Null
            Start-Sleep -Milliseconds 500
            $childBounds=New-Object NativeTrayTest+RECT
            if(![NativeTrayTest]::GetMenuItemRect($window,$trackingMenu,$agentPosition,[ref]$childBounds)){throw 'Подменю отслеживания не открылось'}
            if($childBounds.Right -le $childBounds.Left){throw 'Подменю не получило видимые размеры'}
            [NativeTrayTest]::SetCursorPos(($childBounds.Left+30),(($childBounds.Top+$childBounds.Bottom)/2))|Out-Null
            Start-Sleep -Milliseconds 250
            [NativeTrayTest]::mouse_event(2,0,0,0,[UIntPtr]::Zero);[NativeTrayTest]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
            Start-Sleep -Milliseconds 250
            $saved=Get-Content (Join-Path $temporaryData 'appearance.json') -Raw -Encoding UTF8|ConvertFrom-Json
            if(($agentPosition -eq 0 -and $saved.TrackClaude) -or ($agentPosition -eq 1 -and $saved.TrackCodex)){throw ("Не сохранён переключатель {0}: TrackClaude={1}, TrackCodex={2}; границы подменю {3},{4},{5},{6}" -f $agentPosition,$saved.TrackClaude,$saved.TrackCodex,$childBounds.Left,$childBounds.Top,$childBounds.Right,$childBounds.Bottom)}
            [NativeTrayTest]::SendMessage($window,0x8001,[IntPtr]::Zero,[IntPtr]0x007B)|Out-Null
            Start-Sleep -Milliseconds 300
            $popup=[NativeTrayTest]::FindWindow('#32768',$null)
            $menu=[NativeTrayTest]::SendMessage($popup,0x01E1,[IntPtr]::Zero,[IntPtr]::Zero)
            $trackingMenu=[NativeTrayTest]::GetSubMenu($menu,0)
            $count=[NativeTrayTest]::GetMenuItemCount($menu)
    [NativeTrayTest]::PostMessage($popup,0x0100,[IntPtr]0x24,[IntPtr]::Zero)|Out-Null
    Start-Sleep -Milliseconds 100
        }
    }
    $label=New-Object Text.StringBuilder 128
    [NativeTrayTest]::GetMenuString($menu,($count-1),$label,128,0x0400) | Out-Null
    if($label.ToString() -ne 'Выход'){throw ('Последний пункт: '+$label.ToString())}
    $bounds=New-Object NativeTrayTest+RECT
    [NativeTrayTest]::GetMenuItemRect($window,$menu,($count-1),[ref]$bounds) | Out-Null
    if($bounds.Right -le $bounds.Left){throw "Меню Выход не получило видимые размеры"}
    [NativeTrayTest]::SetCursorPos(($bounds.Left+20),(($bounds.Top+$bounds.Bottom)/2)) | Out-Null
    Start-Sleep -Milliseconds 1000
    if([NativeTrayTest]::FindWindow('#32768',$null) -eq [IntPtr]::Zero){throw 'Меню закрылось при наведении на Выход'}
    [NativeTrayTest]::mouse_event(2,0,0,0,[UIntPtr]::Zero)
    [NativeTrayTest]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
    if(!$testProcess.WaitForExit(5000)){throw 'Выбор Выход мышью не завершил процесс'}
    if($testProcess.ExitCode -ne 0){throw 'Приложение завершилось с ошибкой'}
    'Проверка синхронных событий ЛКМ/ПКМ и выбора Выход мышью пройдена.'
} finally {
    if(!$testProcess.HasExited){$testProcess.Kill();$testProcess.WaitForExit()}
    [NativeTrayTest]::SetCursorPos($originalPoint.X,$originalPoint.Y) | Out-Null
    if($null -eq $previousData){Remove-Item Env:\CWSB_DATA_DIR -ErrorAction SilentlyContinue}else{$env:CWSB_DATA_DIR=$previousData}
    $resolved=[IO.Path]::GetFullPath($temporaryData)
    $base=[IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if($resolved.StartsWith($base,[StringComparison]::OrdinalIgnoreCase)){Remove-Item -LiteralPath $resolved -Recurse -Force}
}
