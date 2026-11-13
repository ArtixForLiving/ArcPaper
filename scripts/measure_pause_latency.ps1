<#
.SYNOPSIS
    Measures and benchmarks ArcPaper auto-pause transition latency.

.DESCRIPTION
    Reads log entries formatted with pause transitions (or evaluates synthetic test events),
    and computes the p50, p90, and p95 latency in milliseconds.
    Outputs a structured summary table and target compliance verdict (p95 <= 150 ms).

.PARAMETER LogPath
    Path to ArcPaper application log file (default: "$env:APPDATA\ArcPaper\app.log").

.PARAMETER SampleCount
    Maximum number of recent transition samples to evaluate (default: 100).

.PARAMETER Synthetic
    Generates synthetic latency samples to verify calculation logic or test without live app logs.

.PARAMETER SimulatePolling
    When -Synthetic is specified, simulates v1.7.0 polling latency (50 - 1000 ms) instead of v1.8.0 target latency.

.EXAMPLE
    .\scripts\measure_pause_latency.ps1 -LogPath "$env:APPDATA\ArcPaper\app.log" -SampleCount 100

.EXAMPLE
    .\scripts\measure_pause_latency.ps1 -Synthetic -SampleCount 100
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$LogPath = "$env:APPDATA\ArcPaper\app.log",

    [Parameter(Position = 1)]
    [int]$SampleCount = 100,

    [switch]$Synthetic,

    [switch]$SimulatePolling
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

$latencies = [System.Collections.Generic.List[double]]::new()
$sourceDescription = ""

if ($Synthetic) {
    $sourceDescription = if ($SimulatePolling) { "Synthetic (v1.7.0 Polling Baseline: 50-1000 ms)" } else { "Synthetic (v1.8.0 Event Coalescer: 35-140 ms)" }
    Write-Host "Generating $SampleCount synthetic pause latency events ($sourceDescription)..." -ForegroundColor Cyan

    $rand = [System.Random]::new(42)
    for ($i = 0; $i -lt $SampleCount; $i++) {
        if ($SimulatePolling) {
            # Polling interval is ~1000 ms; uniform delay between 50 ms and 1000 ms
            $val = 50.0 + ($rand.NextDouble() * 950.0)
        } else {
            # Event-driven coalescer: base 40 ms settle + 5-90 ms processing jitter
            $base = 40.0 + ($rand.NextDouble() * 20.0)
            $jitter = ($rand.NextDouble() * $rand.NextDouble() * 70.0)
            $val = $base + $jitter
        }
        $latencies.Add([Math]::Round($val, 2))
    }
} else {
    $resolvedPath = [System.Environment]::ExpandEnvironmentVariables($LogPath)
    $sourceDescription = $resolvedPath

    if (-not (Test-Path $resolvedPath)) {
        Write-Error "Log file not found at: '$resolvedPath'."
        Write-Host "Tip: Run ArcPaper to produce logs, or use -Synthetic to test calculation logic." -ForegroundColor Yellow
        exit 1
    }

    Write-Host "Parsing pause transition events from '$resolvedPath'..." -ForegroundColor Cyan
    $lines = Get-Content -Path $resolvedPath -ErrorAction Stop

    # Match explicit latency patterns in log lines
    # e.g., "latency: 45ms", "latency=45.2 ms", "transition applied in 48 ms", "duration: 40 ms"
    $regexExplicit = [regex]'(?i)(?:latency|applied in|duration)[=:\s]+(?<val>\d+(?:\.\d+)?)\s*ms'

    # Match paired timestamp transitions if explicit latency is not present:
    # [ts1][INFO] ... event / trigger ... -> [ts2][INFO] ... Smart Auto-Pause ...
    $regexTimestamp = [regex]'^\[(?<ts>\d+)\]\[(?<lvl>[A-Z]+)\]\s*(?<msg>.*)$'

    $pendingTriggerTs = $null

    foreach ($line in $lines) {
        $match = $regexExplicit.Match($line)
        if ($match.Success) {
            $parsedVal = [double]::Parse($match.Groups['val'].Value, [System.Globalization.CultureInfo]::InvariantCulture)
            $latencies.Add([Math]::Round($parsedVal, 2))
            continue
        }

        # Check for paired timestamps
        $tsMatch = $regexTimestamp.Match($line)
        if ($tsMatch.Success) {
            $ts = [double]::Parse($tsMatch.Groups['ts'].Value, [System.Globalization.CultureInfo]::InvariantCulture)
            $msg = $tsMatch.Groups['msg'].Value

            if ($msg -match '(?i)(?:winevent|window_event|coalesce|trigger|coverage evaluate)') {
                $pendingTriggerTs = $ts
            } elseif ($msg -match '(?i)(?:Smart Auto-Pause|pause applied|transition applied)' -and $pendingTriggerTs -ne $null) {
                $diff = $ts - $pendingTriggerTs
                if ($diff -ge 0 -and $diff -le 5000) {
                    $latencies.Add([Math]::Round($diff, 2))
                }
                $pendingTriggerTs = $null
            }
        }
    }

    # Limit to the most recent SampleCount if more are available
    if ($latencies.Count -gt $SampleCount) {
        $startIndex = $latencies.Count - $SampleCount
        $latencies = $latencies.GetRange($startIndex, $SampleCount)
    }
}

if ($latencies.Count -eq 0) {
    Write-Warning "No pause transition events were found in the log."
    Write-Host "Ensure ArcPaper is running and transitions occurred, or pass -Synthetic to verify script calculation." -ForegroundColor Yellow
    exit 0
}

$values = @($latencies)
$min = ($values | Measure-Object -Minimum).Minimum
$max = ($values | Measure-Object -Maximum).Maximum
$avg = [Math]::Round(($values | Measure-Object -Average).Average, 2)
$p50 = Get-Median $values
$p90 = Get-Percentile $values 90
$p95 = Get-Percentile $values 95

$targetThreshold = 150.0
$isPass = ($p95 -le $targetThreshold)
$verdict = if ($isPass) { "PASS (p95 <= 150 ms)" } else { "EXCEEDS_TARGET (p95 > 150 ms)" }
$verdictColor = if ($isPass) { "Green" } else { "Yellow" }

Write-Host ""
Write-Host "==============================================================" -ForegroundColor Cyan
Write-Host " PAUSE TRANSITION LATENCY BENCHMARK" -ForegroundColor Cyan
Write-Host "==============================================================" -ForegroundColor Cyan
Write-Host " Source            : $sourceDescription"
Write-Host " Samples Evaluated : $($latencies.Count)"
Write-Host " Target Threshold  : p95 <= ${targetThreshold} ms (PRD Section 3.3)"
Write-Host "--------------------------------------------------------------" -ForegroundColor DarkGray
Write-Host (" {0,-18} : {1,8:F2} ms" -f "Min Latency", $min)
Write-Host (" {0,-18} : {1,8:F2} ms" -f "p50 (Median)", $p50)
Write-Host (" {0,-18} : {1,8:F2} ms" -f "Average", $avg)
Write-Host (" {0,-18} : {1,8:F2} ms" -f "p90", $p90)
Write-Host (" {0,-18} : {1,8:F2} ms" -f "p95", $p95)
Write-Host (" {0,-18} : {1,8:F2} ms" -f "Max Latency", $max)
Write-Host "--------------------------------------------------------------" -ForegroundColor DarkGray
Write-Host " Target Verdict    : " -NoNewline
Write-Host "$verdict" -ForegroundColor $verdictColor
Write-Host "==============================================================" -ForegroundColor Cyan
Write-Host ""
