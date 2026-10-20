param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('windows', 'tray-command', 'tray-handle', 'window-handle', 'close-window', 'process-info')]
  [string]$Action,
  [Parameter(Mandatory = $true)]
  [int]$RootPid,
  [int]$MenuId,
  [string]$WindowTitle = 'Moe Buddy'
)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class MoeNativeWindow {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr hWnd, StringBuilder sb, int max);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder sb, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", SetLastError = true)] public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
  [DllImport("user32.dll", SetLastError = true)] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
}
"@

function Get-AppWindows {
  $result = New-Object System.Collections.ArrayList
  $callback = {
    param([IntPtr]$hWnd, [IntPtr]$lParam)
    $pidValue = 0
    [MoeNativeWindow]::GetWindowThreadProcessId($hWnd, [ref]$pidValue) | Out-Null
    if ($pidValue -eq $RootPid) {
      $class = New-Object System.Text.StringBuilder 256
      [MoeNativeWindow]::GetClassNameW($hWnd, $class, 256) | Out-Null
      if ($class.ToString() -eq 'Tauri Window') {
        $title = New-Object System.Text.StringBuilder 256
        [MoeNativeWindow]::GetWindowTextW($hWnd, $title, 256) | Out-Null
        [void]$result.Add([pscustomobject]@{
          handle = $hWnd.ToInt64()
          title = $title.ToString()
          visible = [MoeNativeWindow]::IsWindowVisible($hWnd)
        })
      }
    }
    return $true
  }
  [MoeNativeWindow]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
  return @($result)
}

function Get-MainWindow {
  $script:mainHandle = [IntPtr]::Zero
  $callback = {
    param([IntPtr]$hWnd, [IntPtr]$lParam)
    $pidValue = 0
    [MoeNativeWindow]::GetWindowThreadProcessId($hWnd, [ref]$pidValue) | Out-Null
    if ($pidValue -eq $RootPid) {
      $class = New-Object System.Text.StringBuilder 256
      [MoeNativeWindow]::GetClassNameW($hWnd, $class, 256) | Out-Null
      if ($class.ToString() -eq 'Tauri Window') {
        $title = New-Object System.Text.StringBuilder 256
        [MoeNativeWindow]::GetWindowTextW($hWnd, $title, 256) | Out-Null
        if ($title.ToString() -eq $WindowTitle) {
          $script:mainHandle = $hWnd
        }
      }
    }
    return $true
  }
  [MoeNativeWindow]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
  return $script:mainHandle
}

function Get-TrayWindow {
  $script:trayHandle = [IntPtr]::Zero
  $callback = {
    param([IntPtr]$hWnd, [IntPtr]$lParam)
    $pidValue = 0
    [MoeNativeWindow]::GetWindowThreadProcessId($hWnd, [ref]$pidValue) | Out-Null
    if ($pidValue -eq $RootPid) {
      $class = New-Object System.Text.StringBuilder 256
      [MoeNativeWindow]::GetClassNameW($hWnd, $class, 256) | Out-Null
      if ($class.ToString() -eq 'tray_icon_app') {
        $script:trayHandle = $hWnd
      }
    }
    return $true
  }
  [MoeNativeWindow]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
  return $trayHandle
}

try {
  switch ($Action) {
    'windows' {
      @{ ok = $true; windows = @(Get-AppWindows) } | ConvertTo-Json -Compress -Depth 5
    }
    'window-handle' {
      $handle = Get-MainWindow
      if ($null -eq $handle -or $handle -eq [IntPtr]::Zero) {
        @{ ok = $false; error = 'main window not found' } | ConvertTo-Json -Compress
        exit 1
      }
      @{ ok = $true; handle = $handle.ToInt64() } | ConvertTo-Json -Compress
    }
    'close-window' {
      $handle = Get-MainWindow
      if ($null -eq $handle -or $handle -eq [IntPtr]::Zero) {
        @{ ok = $false; error = 'main window not found' } | ConvertTo-Json -Compress
        exit 1
      }
      # WM_CLOSE 走真实原生关窗事件；不绕过宿主 CloseRequested 协调。
      $posted = [MoeNativeWindow]::PostMessageW([IntPtr]$handle.ToInt64(), 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
      @{ ok = [bool]$posted; handle = $handle.ToInt64() } | ConvertTo-Json -Compress
    }
    'tray-handle' {
      $tray = Get-TrayWindow
      if ($null -eq $tray -or $tray -eq [IntPtr]::Zero) {
        @{ ok = $false; error = 'tray window not found' } | ConvertTo-Json -Compress
        exit 1
      }
      @{ ok = $true; handle = $tray.ToInt64() } | ConvertTo-Json -Compress
    }
    'process-info' {
      # 清理前核对 PID 对应的可执行文件与命令行，避免误伤其他进程。
      $process = Get-CimInstance Win32_Process -Filter "ProcessId=$RootPid" -ErrorAction Stop
      if ($null -eq $process) {
        @{ ok = $false; error = 'process not found'; pid = $RootPid } | ConvertTo-Json -Compress
        exit 1
      }
      @{
        ok = $true
        pid = [int]$process.ProcessId
        executablePath = $process.ExecutablePath
        commandLine = $process.CommandLine
      } | ConvertTo-Json -Compress -Depth 5
    }
    'tray-command' {
      if (-not $PSBoundParameters.ContainsKey('MenuId')) {
        @{ ok = $false; error = 'MenuId is required' } | ConvertTo-Json -Compress
        exit 1
      }
      $script:trayHandle = [IntPtr]::Zero
      $trayCallback = {
        param([IntPtr]$hWnd, [IntPtr]$lParam)
        $pidValue = 0
        [MoeNativeWindow]::GetWindowThreadProcessId($hWnd, [ref]$pidValue) | Out-Null
        if ($pidValue -eq $RootPid) {
          $class = New-Object System.Text.StringBuilder 256
          [MoeNativeWindow]::GetClassNameW($hWnd, $class, 256) | Out-Null
          if ($class.ToString() -eq 'tray_icon_app') { $script:trayHandle = $hWnd }
        }
        return $true
      }
      [MoeNativeWindow]::EnumWindows($trayCallback, [IntPtr]::Zero) | Out-Null
      $tray = $script:trayHandle
      if ($null -eq $tray -or $tray -eq [IntPtr]::Zero) {
        @{ ok = $false; error = 'tray window not found' } | ConvertTo-Json -Compress
        exit 1
      }
      # WM_COMMAND 的最小 16 位是菜单项 ID；这是托盘原生菜单事件路径。
      # 显式归一化为 IntPtr，避免脚本作用域包装对象影响 P/Invoke 参数封送。
      $tray = [IntPtr]$tray.ToInt64()
      $posted = [MoeNativeWindow]::PostMessageW($tray, 0x0111, [IntPtr]$MenuId, [IntPtr]::Zero)
      @{ ok = [bool]$posted; handle = $tray.ToInt64(); menuId = $MenuId } | ConvertTo-Json -Compress
    }
  }
} catch {
  @{ ok = $false; error = $_.Exception.Message } | ConvertTo-Json -Compress
  exit 1
}
