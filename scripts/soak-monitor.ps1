# SnipStack 浸泡测试监控脚本(Windows,pwsh 7+)。
#
# 职责:启动 app → 持续模拟剪贴板写入(文本/大文本/文件)→ 周期采样进程
# GDI / USER / 句柄 / 内存并落 CSV → 定期强杀重启 explorer(复现 TaskbarCreated
# 场景)→ 结束时做断言:
#   1. 进程仍然存活;
#   2. GDI / USER 未逼近 10000 上限(阈值可调);
#   3. GDI / USER / 句柄没有持续泄漏趋势(尾部均值 vs 头部均值);
#   4. app 日志里没有 watchdog 卡死记录(diagnostics: main thread unresponsive)
#      和 panic 记录(panic on thread)。
# 任一断言失败以非零码退出,CI 由此判红。CSV 与 app 日志由工作流上传为 artifact。
#
# 本地(真实 Win11)也可直接跑:
#   pwsh scripts/soak-monitor.ps1 -ExePath "C:\...\SnipStack.exe" -DurationMinutes 60

param(
    [Parameter(Mandatory = $true)][string]$ExePath,
    [int]$DurationMinutes = 120,
    [string]$LogDir = "$env:LOCALAPPDATA\com.snipstack.app\logs",
    [string]$CsvPath = "soak-samples.csv",
    # 每隔多少分钟强杀并重启 explorer;0 = 关闭该场景。
    [int]$ExplorerRestartEveryMinutes = 15,
    [int]$GdiLimit = 8000,
    [int]$UserLimit = 8000
)

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class SoakNative {
    [DllImport("user32.dll")]
    public static extern uint GetGuiResources(IntPtr hProcess, uint uiFlags);
}
"@

function Get-Sample([System.Diagnostics.Process]$proc) {
    $proc.Refresh()
    [pscustomobject]@{
        Timestamp    = (Get-Date -Format 'o')
        Gdi          = [SoakNative]::GetGuiResources($proc.Handle, 0)
        User         = [SoakNative]::GetGuiResources($proc.Handle, 1)
        Handles      = $proc.HandleCount
        WorkingSetMB = [math]::Round($proc.WorkingSet64 / 1MB, 1)
        PrivateMB    = [math]::Round($proc.PrivateMemorySize64 / 1MB, 1)
    }
}

Write-Host "Starting $ExePath for $DurationMinutes minutes (explorer restart every $ExplorerRestartEveryMinutes min)"
$app = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds 20
if ($app.HasExited) { throw "app exited during startup, exit code $($app.ExitCode)" }

# 准备一批用于「复制文件」场景的真实文件(触发文件图标提取,GDI 敏感路径)。
# pwsh 7 的 Set-Clipboard 只支持文本,文件列表走 WinForms 的 SetFileDropList(需 STA,
# pwsh 在 Windows 上默认即 STA;失败时降级为跳过文件场景)。
Add-Type -AssemblyName System.Windows.Forms
$fileDropList = New-Object System.Collections.Specialized.StringCollection
Get-ChildItem -Path $PSScriptRoot -File |
    Select-Object -First 5 -ExpandProperty FullName |
    ForEach-Object { [void]$fileDropList.Add($_) }

$samples = New-Object System.Collections.Generic.List[object]
$deadline = (Get-Date).AddMinutes($DurationMinutes)
$lastSample = [datetime]::MinValue
$lastExplorerRestart = Get-Date
$iteration = 0
$failures = New-Object System.Collections.Generic.List[string]

"Timestamp,Gdi,User,Handles,WorkingSetMB,PrivateMB" | Set-Content -Path $CsvPath

while ((Get-Date) -lt $deadline) {
    $iteration++

    # 剪贴板负载:普通文本 / 大文本 / 文件列表轮换。
    try {
        if ($iteration % 4 -eq 0 -and $fileDropList.Count -gt 0) {
            [System.Windows.Forms.Clipboard]::SetFileDropList($fileDropList)
        } elseif ($iteration % 10 -eq 0) {
            Set-Clipboard -Value ("soak-large-{0}-{1}" -f $iteration, ('x' * 200000))
        } else {
            Set-Clipboard -Value ("soak-{0}-{1}-{2}" -f $iteration, [guid]::NewGuid(), ('y' * (Get-Random -Maximum 2000)))
        }
    } catch {
        Write-Warning "Set-Clipboard failed at iteration ${iteration}: $_"
    }

    # 每 30s 采样一次。
    if (((Get-Date) - $lastSample).TotalSeconds -ge 30) {
        $lastSample = Get-Date
        if ($app.HasExited) {
            $failures.Add("app process exited mid-run at iteration $iteration, exit code $($app.ExitCode)")
            break
        }
        $s = Get-Sample $app
        $samples.Add($s)
        "$($s.Timestamp),$($s.Gdi),$($s.User),$($s.Handles),$($s.WorkingSetMB),$($s.PrivateMB)" | Add-Content -Path $CsvPath
        if ($samples.Count % 10 -eq 0) {
            Write-Host "[$($s.Timestamp)] gdi=$($s.Gdi) user=$($s.User) handles=$($s.Handles) ws=$($s.WorkingSetMB)MB"
        }
    }

    # 周期性强杀 explorer,验证 TaskbarCreated 后托盘图标重建 + app 不受牵连。
    if ($ExplorerRestartEveryMinutes -gt 0 -and
        ((Get-Date) - $lastExplorerRestart).TotalMinutes -ge $ExplorerRestartEveryMinutes) {
        $lastExplorerRestart = Get-Date
        Write-Host "Restarting explorer..."
        Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 5
        if (-not (Get-Process -Name explorer -ErrorAction SilentlyContinue)) {
            Start-Process explorer.exe
        }
        Start-Sleep -Seconds 10
    }

    Start-Sleep -Seconds 5
}

# ---- 断言 ----

if (-not $app.HasExited) {
    Write-Host "Run finished, app still alive."
} elseif ($failures.Count -eq 0) {
    $failures.Add("app process exited before run finished, exit code $($app.ExitCode)")
}

if ($samples.Count -ge 4) {
    $head = $samples | Select-Object -First ([math]::Min(10, [int]($samples.Count / 2)))
    $tail = $samples | Select-Object -Last ([math]::Min(10, [int]($samples.Count / 2)))
    foreach ($metric in 'Gdi', 'User', 'Handles') {
        $first = ($head | Measure-Object -Property $metric -Average).Average
        $last = ($tail | Measure-Object -Property $metric -Average).Average
        Write-Host ("{0}: head avg {1:n0} -> tail avg {2:n0}" -f $metric, $first, $last)
        # 泄漏趋势:尾部均值超过头部均值 3 倍且绝对增量超 1000,判定为疑似泄漏。
        if ($last -gt $first * 3 + 1000) {
            $failures.Add("suspected $metric leak: head avg $([math]::Round($first)) -> tail avg $([math]::Round($last))")
        }
    }
    $final = $samples[-1]
    if ($final.Gdi -ge $GdiLimit) { $failures.Add("GDI objects near exhaustion: $($final.Gdi) >= $GdiLimit") }
    if ($final.User -ge $UserLimit) { $failures.Add("USER objects near exhaustion: $($final.User) >= $UserLimit") }
}

# 扫描 app 自带黑匣子日志:watchdog 卡死记录与 panic。
if (Test-Path $LogDir) {
    $hits = Get-ChildItem -Path $LogDir -Filter *.log |
        Select-String -Pattern 'main thread unresponsive', 'panic on thread'
    foreach ($hit in $hits) {
        $failures.Add("app log: $($hit.Line.Trim())")
    }
    $dumps = Get-ChildItem -Path $LogDir -Filter 'hang-*.dmp' -ErrorAction SilentlyContinue
    foreach ($dump in $dumps) {
        $failures.Add("hang minidump present: $($dump.Name)")
    }
} else {
    $failures.Add("app log dir not found: $LogDir (app never initialized logging?)")
}

if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue }

if ($failures.Count -gt 0) {
    Write-Host "`n=== SOAK FAILURES ===" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host " - $_" -ForegroundColor Red }
    exit 1
}
Write-Host "`nSoak test passed: $($samples.Count) samples, no leak trend, no watchdog/panic records."
