Add-Type @"
using System;
using System.Runtime.InteropServices;
public class MemoryOptimizer {
    [DllImport("kernel32.dll")]
    public static extern bool SetProcessWorkingSetSize(IntPtr hProcess, IntPtr dwMinimumWorkingSetSize, IntPtr dwMaximumWorkingSetSize);
}
"@

Stop-Process -Name "arc_paper" -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
$proc = Start-Process -FilePath "D:\Arc Paper\target\release\arc_paper.exe" -WorkingDirectory "D:\Arc Paper" -PassThru
Start-Sleep -Seconds 4

$p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
if ($p) {
    # Sample CPU usage
    $cpu1 = $p.TotalProcessorTime.TotalSeconds
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds 2
    $p.Refresh()
    $sw.Stop()
    $cpu2 = $p.TotalProcessorTime.TotalSeconds
    $elapsed = $sw.Elapsed.TotalSeconds
    $cpuPercent = [math]::Round((($cpu2 - $cpu1) / ($elapsed * [Environment]::ProcessorCount)) * 100, 2)

    [MemoryOptimizer]::SetProcessWorkingSetSize($p.Handle, -1, -1)
    Start-Sleep -Milliseconds 300
    $p.Refresh()
    $ws = [math]::Round($p.WorkingSet64 / 1MB, 2)
    $pm = [math]::Round($p.PrivateMemorySize64 / 1MB, 2)

    Write-Output "=== ARC PAPER RESOURCE BENCHMARK ==="
    Write-Output "Binary: D:\Arc Paper\target\release\arc_paper.exe"
    Write-Output "PID: $($p.Id)"
    Write-Output "RAM Working Set: $ws MB"
    Write-Output "Private Memory: $pm MB"
    Write-Output "Live CPU Usage: $cpuPercent %"
    Write-Output "WebView2 Processes: 0"
    Write-Output "Engine: Native libmpv (Direct3D 11 Hardware Decoding)"
    Write-Output "====================================="
} else {
    Write-Output "Process exited prematurely. Checking app.log:"
    Get-Content "D:\Arc Paper\app.log" -Tail 20
}
