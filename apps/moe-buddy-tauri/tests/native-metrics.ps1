param(
  [Parameter(Mandatory = $true)]
  [int]$RootPid,
  [Parameter(Mandatory = $true)]
  [string]$Label,
  [int]$Seconds = 2
)

function Get-ProcessTree([int]$rootPid) {
  $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, CommandLine)
  $children = @{}
  foreach ($process in $all) {
    $parent = [int]$process.ParentProcessId
    if (-not $children.ContainsKey($parent)) {
      $children[$parent] = New-Object System.Collections.ArrayList
    }
    [void]$children[$parent].Add($process)
  }
  $result = New-Object System.Collections.ArrayList
  $queue = New-Object System.Collections.Queue
  $queue.Enqueue([int]$rootPid)
  $seen = @{}
  while ($queue.Count -gt 0) {
    $pidValue = [int]$queue.Dequeue()
    if ($seen.ContainsKey($pidValue)) { continue }
    $seen[$pidValue] = $true
    $process = $all | Where-Object { $_.ProcessId -eq $pidValue } | Select-Object -First 1
    if ($null -ne $process -and ($process.Name -eq 'moe-buddy-tauri.exe' -or $process.Name -eq 'msedgewebview2.exe')) {
      [void]$result.Add($process)
    }
    if ($children.ContainsKey($pidValue)) {
      foreach ($child in $children[$pidValue]) {
        if (-not $seen.ContainsKey([int]$child.ProcessId)) {
          $queue.Enqueue([int]$child.ProcessId)
        }
      }
    }
  }
  return @($result)
}

function Get-Role($process) {
  if ($process.Name -eq 'moe-buddy-tauri.exe') { return 'host' }
  if ($process.CommandLine -match '--type=([a-zA-Z-]+)') {
    return "webview2:$($Matches[1])"
  }
  return 'webview2:browser'
}

try {
  $processes = @(Get-ProcessTree -rootPid $RootPid)
  if ($processes.Count -eq 0) {
    @{ ok = $false; error = 'no matching process tree' } | ConvertTo-Json -Compress
    exit 1
  }
  $ids = @($processes | ForEach-Object { [int]$_.ProcessId })
  $roles = @{}
  foreach ($process in $processes) {
    $roles[[int]$process.ProcessId] = Get-Role $process
  }

  $startCpu = @{}
  $start = Get-Date
  foreach ($id in $ids) {
    $live = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($null -ne $live) {
      $startCpu[$id] = $live.TotalProcessorTime.TotalSeconds
    }
  }
  $lastCpu = @{}
  $peakWorking = @{}
  $sampleCount = [Math]::Max(1, $Seconds * 4)
  $workingSums = @{}
  for ($index = 0; $index -lt $sampleCount; $index++) {
    foreach ($id in $ids) {
      $live = Get-Process -Id $id -ErrorAction SilentlyContinue
      if ($null -eq $live) { continue }
      $lastCpu[$id] = $live.TotalProcessorTime.TotalSeconds
      $working = [double]$live.WorkingSet64
      if (-not $workingSums.ContainsKey($id)) { $workingSums[$id] = 0.0 }
      $workingSums[$id] += $working
      if (-not $peakWorking.ContainsKey($id) -or $working -gt $peakWorking[$id]) {
        $peakWorking[$id] = $working
      }
    }
    Start-Sleep -Milliseconds 250
  }
  $elapsed = [Math]::Max(0.001, ((Get-Date) - $start).TotalSeconds)
  $logical = [int]$env:NUMBER_OF_PROCESSORS
  $groups = @{}
  $processRows = New-Object System.Collections.ArrayList
  foreach ($id in $ids) {
    $role = $roles[$id]
    $prefix = if ($role -eq 'host') { 'host' } else { 'webview' }
    if (-not $groups.ContainsKey($prefix)) {
      $groups[$prefix] = @{ cpuSeconds = 0.0; workingSum = 0.0; workingPeak = 0.0; count = 0 }
    }
    $cpuDelta = 0.0
    if ($startCpu.ContainsKey($id) -and $lastCpu.ContainsKey($id)) {
      $cpuDelta = [double]($lastCpu[$id] - $startCpu[$id])
    }
    $group = $groups[$prefix]
    $group.cpuSeconds += $cpuDelta
    if ($workingSums.ContainsKey($id)) {
      $averageWorking = $workingSums[$id] / $sampleCount
      $group.workingSum += $averageWorking
      $group.workingPeak += $peakWorking[$id]
      $group.count += 1
      $live = Get-Process -Id $id -ErrorAction SilentlyContinue
      [void]$processRows.Add([pscustomobject]@{
        pid = $id
        role = $role
        workingSetMB = [Math]::Round($averageWorking / 1MB, 2)
        peakWorkingSetMB = [Math]::Round($peakWorking[$id] / 1MB, 2)
        privateMB = if ($null -ne $live) { [Math]::Round($live.PrivateMemorySize64 / 1MB, 2) } else { $null }
        cpuSeconds = [Math]::Round($cpuDelta, 4)
      })
    }
  }
  $report = @{
    ok = $true
    label = $Label
    seconds = [Math]::Round($elapsed, 3)
    logicalProcessors = $logical
    processCount = $processRows.Count
    processes = @($processRows)
  }
  foreach ($key in @('host', 'webview')) {
    if ($groups.ContainsKey($key)) {
      $group = $groups[$key]
      $report[$key] = @{
        cpuPercent = [Math]::Round(($group.cpuSeconds / $elapsed / $logical) * 100, 2)
        workingSetMB = [Math]::Round($group.workingSum / 1MB, 2)
        peakWorkingSetMB = [Math]::Round($group.workingPeak / 1MB, 2)
      }
    } else {
      $report[$key] = @{ cpuPercent = 0; workingSetMB = 0; peakWorkingSetMB = 0 }
    }
  }
  $report | ConvertTo-Json -Compress -Depth 6
} catch {
  @{ ok = $false; error = $_.Exception.Message } | ConvertTo-Json -Compress
  exit 1
}
