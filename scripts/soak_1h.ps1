<#
.SYNOPSIS
    Executes a 1-hour stability and memory leak soak test for ArcPaper.

.DESCRIPTION
    Monitors the target process for a specified duration (default: 60 minutes),
    sampling Working Set RAM, Private Memory, Thread Count, and CPU utilization.
    Calculates memory drift ([End RAM - Start RAM]) and evaluates whether the process
    meets the stability criterion of remaining within +- 5.0 MB without crashing.
    Exports all collected samples to a CSV file.

.PARAMETER ProcessName
    Target process name without .exe extension (default: "arc_paper").

.PARAMETER DurationMinutes
    Total soak test duration in minutes (default: 60).

.PARAMETER IntervalSeconds
    Interval between consecutive samples in seconds (default: 60).

.PARAMETER OutputFile
    Path to write the CSV report (default: "docs/baselines/soak_1h_result.csv").

.EXAMPLE
    .\scripts\soak_1h.ps1 -ProcessName arc_paper -DurationMinutes 60 -IntervalSeconds 60 -OutputFile docs\baselines\soak_1h_result.csv
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$ProcessName = "arc_paper",

    [Parameter(Position = 1)]
    [int]$DurationMinutes = 60,

    [Parameter(Position = 2)]
    [int]$IntervalSeconds = 60,

    [Parameter(Position = 3)]
    [string]$OutputFile = "docs/baselines/soak_1h_result.csv",

    [Parameter(Position = 4)]
    [int]$MaxSamples = 0
)

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

if ($DurationMinutes -lt 1) {
    Write-Warning "DurationMinutes must be at least 1. Resetting to 1."
    $DurationMinutes = 1
}

$totalSeconds = $DurationMinutes * 60
$totalSamples = if ($MaxSamples -gt 0) { $MaxSamples } else { [Math]::Max(1, [int][Math]::Ceiling($totalSeconds / $IntervalSeconds)) }

Write-Host "Looking for process '$ProcessName'..." -ForegroundColor Cyan
$process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1

while (-not $process -or $process.HasExited) {
    Write-Host "Process '$ProcessName' is not running. Waiting 5 seconds (Press Ctrl+C to abort)..." -ForegroundColor Yellow
    Start-Sleep -Seconds 5
    $process = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
}

Write-Host "Attached to '$($process.ProcessName)' (PID: $($process.Id))." -ForegroundColor Green
Write-Host "Starting Soak Test: Duration = ${DurationMinutes} minutes ($totalSamples samples @ every ${IntervalSeconds}s)..." -ForegroundColor Cyan

$logicalCores = [System.Environment]::ProcessorCount
$process.Refresh()
$initialWsMb = [Math]::Round($process.WorkingSet64 / 1MB, 2)
$initialPmMb = [Math]::Round($process.PrivateMemorySize64 / 1MB, 2)
$lastCpuTime = $process.TotalProcessorTime.TotalSeconds
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$testStartTime = Get-Date

Write-Host ("Initial Baseline RAM: WorkingSet = {0:F2} MB | PrivateMemory = {1:F2} MB" -f $initialWsMb, $initialPmMb) -ForegroundColor Cyan

$samples = [System.Collections.Generic.List[PSObject]]::new()
$processCrashed = $false

try {
    for ($i = 1; $i -le $totalSamples; $i++) {
        Start-Sleep -Seconds $IntervalSeconds

        if ($process.HasExited) {
            Write-Error "CRITICAL: Process '$ProcessName' (PID: $($process.Id)) terminated unexpectedly during soak test!"
            $processCrashed = $true
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
        $driftMb = [Math]::Round($wsMb - $initialWsMb, 2)
        $timestamp = (Get-Date).ToString("o")

        $sample = [PSCustomObject]@{
            SampleIndex     = $i
            Timestamp       = $timestamp
            WorkingSetMB    = $wsMb
            PrivateMemoryMB = $pmMb
            DriftMB         = $driftMb
            CpuPercent      = $cpuPercent
            ThreadCount     = $threads
        }
        $samples.Add($sample)

        $driftSign = if ($driftMb -ge 0) { "+$driftMb" } else { "$driftMb" }
        Write-Host ("[{0,3}/{1,3}] Time: {2} | RAM: {3,6:F2} MB (Drift: {4,7} MB) | CPU: {5,5:F2}% | Threads: {6}" -f `
            $i, $totalSamples, (Get-Date -Format "HH:mm:ss"), $wsMb, $driftSign, $cpuPercent, $threads)
    }
}
finally {
    if ($samples.Count -gt 0) {
        $wsValues = @($samples | ForEach-Object { [double]$_.WorkingSetMB })
        $cpuValues = @($samples | ForEach-Object { [double]$_.CpuPercent })

        $startRam = $initialWsMb
        $endRam = $wsValues[-1]
        $finalDrift = [Math]::Round($endRam - $startRam, 2)
        $absDrift = [Math]::Abs($finalDrift)

        $wsMin = ($wsValues | Measure-Object -Minimum).Minimum
        $wsMax = ($wsValues | Measure-Object -Maximum).Maximum
        $wsMed = Get-Median $wsValues

        $cpuAvg = [Math]::Round(($cpuValues | Measure-Object -Average).Average, 2)
        $cpuMax = ($cpuValues | Measure-Object -Maximum).Maximum

        $driftPassed = ($absDrift -le 5.0)
        $overallPassed = $driftPassed -and (-not $processCrashed)

        $verdict = if ($overallPassed) { "PASS" } else { "FAIL" }
        $verdictColor = if ($overallPassed) { "Green" } else { "Red" }
        $elapsedMins = [Math]::Round(((Get-Date) - $testStartTime).TotalMinutes, 1)

        Write-Host ""
        Write-Host "==============================================================" -ForegroundColor Cyan
        Write-Host " SOAK TEST REPORT: $($process.ProcessName)" -ForegroundColor Cyan
        Write-Host "==============================================================" -ForegroundColor Cyan
        Write-Host " Samples Collected : $($samples.Count) / $totalSamples"
        Write-Host " Elapsed Time      : ${elapsedMins} minutes"
        Write-Host " Process Status    : $(if ($processCrashed) { 'CRASHED / TERMINATED' } else { 'HEALTHY' })"
        Write-Host "--------------------------------------------------------------" -ForegroundColor DarkGray
        Write-Host (" Start RAM (WS)    : {0:F2} MB" -f $startRam)
        Write-Host (" End RAM (WS)      : {0:F2} MB" -f $endRam)
        Write-Host (" Memory Drift      : {0:+0.00;-0.00;0.00} MB (Criterion: |drift| <= 5.00 MB)" -f $finalDrift)
        Write-Host (" RAM Range         : Min = {0:F2} MB | Median = {1:F2} MB | Max = {2:F2} MB" -f $wsMin, $wsMed, $wsMax)
        Write-Host (" CPU Utilization   : Average = {0:F2}% | Max = {1:F2}%" -f $cpuAvg, $cpuMax)
        Write-Host "--------------------------------------------------------------" -ForegroundColor DarkGray
        Write-Host " Final Verdict     : " -NoNewline
        Write-Host "$verdict" -ForegroundColor $verdictColor
        Write-Host "==============================================================" -ForegroundColor Cyan
        Write-Host ""

        if (-not [string]::IsNullOrWhiteSpace($OutputFile)) {
            $parentDir = Split-Path -Parent $OutputFile
            if (-not [string]::IsNullOrWhiteSpace($parentDir) -and -not (Test-Path $parentDir)) {
                New-Item -ItemType Directory -Path $parentDir -Force | Out-Null
            }
            $samples | Export-Csv -Path $OutputFile -NoTypeInformation -Encoding utf8
            Write-Host "Soak test records successfully saved to: $OutputFile" -ForegroundColor Green
        }
    } else {
        Write-Warning "No soak test samples were recorded."
    }
}
