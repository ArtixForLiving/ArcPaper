<#
.SYNOPSIS
    Measures ArcPaper process resource usage (Working Set, Private Memory, CPU %, Threads).

.DESCRIPTION
    Samples process performance metrics at regular intervals for a specified duration.
    Calculates median, min, max for memory, and median, p95, max for CPU usage.
    Exports sample data to CSV if an output path is provided.

.PARAMETER ProcessName
    Target process name without .exe (default: "arc_paper").

.PARAMETER DurationSeconds
    Measurement duration in seconds (default: 600).

.PARAMETER IntervalSeconds
    Sampling interval in seconds (default: 1).

.PARAMETER OutputFile
    Optional file path to export sampled records as CSV.

.EXAMPLE
    .\scripts\measure_resource.ps1 -ProcessName arc_paper -DurationSeconds 600 -IntervalSeconds 1 -OutputFile docs\baselines\active_1mon.csv
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$ProcessName = "arc_paper",

    [Parameter(Position = 1)]
    [int]$DurationSeconds = 600,

    [Parameter(Position = 2)]
    [int]$IntervalSeconds = 1,

    [Parameter(Position = 3)]
    [string]$OutputFile = ""
)

function Get-Percentile {
    param(
        [Parameter(Mandatory = $true)]
        [double[]]$Values,
        [Parameter(Mandatory = $true)]
        [double]$Percentile
    )
    if ($Values.Count -eq 0) { return 0.0 }
    $sorted = $Values | Sort-Object
    $count = $sorted.Count
    if ($count -eq 1) { return [Math]::Round($sorted[0], 2) }

    $index = [Math]::Ceiling(($Percentile / 100.0) * $count) - 1
    if ($index -lt 0) { $index = 0 }
    if ($index -ge $count) { $index = $count - 1 }
    return [Math]::Round($sorted[$index], 2)
}

function Get-Median {
    param(
        [Parameter(Mandatory = $true)]
        [double[]]$Values
    )
    if ($Values.Count -eq 0) { return 0.0 }
    $sorted = $Values | Sort-Object
    $count = $sorted.Count
    if ($count % 2 -eq 1) {
        $mid = [Math]::Floor($count / 2)
        return [Math]::Round($sorted[$mid], 2)
    } else {
        $mid2 = [int]($count / 2)
        $mid1 = $mid2 - 1
        return [Math]::Round(($sorted[$mid1] + $sorted[$mid2]) / 2.0, 2)
    }
}

if ($IntervalSeconds -lt 1) {
    Write-Warning "IntervalSeconds must be at least 1. Resetting to 1."
    $IntervalSeconds = 1
}

if ($DurationSeconds -lt $IntervalSeconds) {
    $DurationSeconds = $IntervalSeconds
}

Write-Host "Looking for process '$ProcessName'..." -ForegroundColor Cyan
$process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1

while (-not $process -or $process.HasExited) {
    Write-Host "Process '$ProcessName' is not running. Waiting 2 seconds (Press Ctrl+C to abort)..." -ForegroundColor Yellow
    Start-Sleep -Seconds 2
    $process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
}

Write-Host "Attached to '$($process.ProcessName)' (PID: $($process.Id))." -ForegroundColor Green
Write-Host "Starting measurement: Duration = ${DurationSeconds}s, Interval = ${IntervalSeconds}s..." -ForegroundColor Cyan

$logicalCores = [System.Environment]::ProcessorCount
$process.Refresh()
$lastCpuTime = $process.TotalProcessorTime.TotalSeconds
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$startTime = Get-Date
$samples = [System.Collections.Generic.List[PSObject]]::new()
$totalSamples = [Math]::Max(1, [int][Math]::Floor($DurationSeconds / $IntervalSeconds))

try {
    for ($i = 1; $i -le $totalSamples; $i++) {
        Start-Sleep -Seconds $IntervalSeconds

        if ($process.HasExited) {
            Write-Warning "Process '$ProcessName' (PID: $($process.Id)) terminated during measurement."
            break
        }

        $process.Refresh()
        $elapsedSec = $stopwatch.Elapsed.TotalSeconds
        $currentCpuTime = $process.TotalProcessorTime.TotalSeconds
        $deltaCpu = $currentCpuTime - $lastCpuTime
        $stopwatch.Restart()
        $lastCpuTime = $currentCpuTime

        $cpuPercent = 0.0
        if ($elapsedSec -gt 0 -and $logicalCores -gt 0) {
            $rawCpu = ($deltaCpu / ($elapsedSec * $logicalCores)) * 100.0
            $cpuPercent = [Math]::Max(0.0, [Math]::Round($rawCpu, 2))
        }

        $wsMb = [Math]::Round($process.WorkingSet64 / 1MB, 2)
        $pmMb = [Math]::Round($process.PrivateMemorySize64 / 1MB, 2)
        $threads = $process.Threads.Count
        $timestamp = (Get-Date).ToString("o")

        $sample = [PSCustomObject]@{
            SampleIndex     = $i
            Timestamp       = $timestamp
            WorkingSetMB    = $wsMb
            PrivateMemoryMB = $pmMb
            CpuPercent      = $cpuPercent
            ThreadCount     = $threads
        }
        $samples.Add($sample)

        Write-Host ("[{0}/{1}] Time: {2} | RAM: {3,6} MB (Priv: {4,6} MB) | CPU: {5,5}% | Threads: {6}" -f `
            $i, $totalSamples, (Get-Date -Format "HH:mm:ss"), $wsMb, $pmMb, $cpuPercent, $threads)
    }
}
finally {
    if ($samples.Count -gt 0) {
        $wsValues = @($samples | ForEach-Object { [double]$_.WorkingSetMB })
        $pmValues = @($samples | ForEach-Object { [double]$_.PrivateMemoryMB })
        $cpuValues = @($samples | ForEach-Object { [double]$_.CpuPercent })

        $wsMin = ($wsValues | Measure-Object -Minimum).Minimum
        $wsMax = ($wsValues | Measure-Object -Maximum).Maximum
        $wsMed = Get-Median $wsValues

        $pmMin = ($pmValues | Measure-Object -Minimum).Minimum
        $pmMax = ($pmValues | Measure-Object -Maximum).Maximum
        $pmMed = Get-Median $pmValues

        $cpuMin = ($cpuValues | Measure-Object -Minimum).Minimum
        $cpuMax = ($cpuValues | Measure-Object -Maximum).Maximum
        $cpuMed = Get-Median $cpuValues
        $cpuP95 = Get-Percentile $cpuValues 95

        $elapsedTotal = [Math]::Round(((Get-Date) - $startTime).TotalSeconds, 1)

        Write-Host "`n========================================================" -ForegroundColor Cyan
        Write-Host " RESOURCE USAGE SUMMARY: $($process.ProcessName)" -ForegroundColor Cyan
        Write-Host "========================================================" -ForegroundColor Cyan
        Write-Host " Samples Collected : $($samples.Count)"
        Write-Host " Elapsed Time      : ${elapsedTotal}s"
        Write-Host (" Working Set (MB)  : Median = {0:F2} MB | Min = {1:F2} MB | Max = {2:F2} MB" -f $wsMed, $wsMin, $wsMax)
        Write-Host (" Private Mem (MB)  : Median = {0:F2} MB | Min = {1:F2} MB | Max = {2:F2} MB" -f $pmMed, $pmMin, $pmMax)
        Write-Host (" CPU Utilization   : Median = {0:F2}%  | p95 = {1:F2}%  | Max = {2:F2}%" -f $cpuMed, $cpuP95, $cpuMax)
        Write-Host "========================================================`n" -ForegroundColor Cyan

        if (-not [string]::IsNullOrWhiteSpace($OutputFile)) {
            $parentDir = Split-Path -Parent $OutputFile
            if (-not [string]::IsNullOrWhiteSpace($parentDir) -and -not (Test-Path $parentDir)) {
                New-Item -ItemType Directory -Path $parentDir -Force | Out-Null
            }
            $samples | Export-Csv -Path $OutputFile -NoTypeInformation -Encoding utf8
            Write-Host "Sample records successfully exported to: $OutputFile" -ForegroundColor Green
        }
    } else {
        Write-Warning "No samples were collected."
    }
}
