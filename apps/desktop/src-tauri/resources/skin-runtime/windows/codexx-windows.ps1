[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('inspect', 'selectPort', 'launch', 'stopCodex', 'launchNormal', 'verifyPort', 'processInfo', 'injectorStatus', 'stopInjector')]
  [string]$Action,
  [int]$Port = 9341,
  [int]$TargetPid = 0,
  [string]$StatePath
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. (Join-Path $PSScriptRoot 'common-windows.ps1')

function Write-CodexxJson {
  param([Parameter(Mandatory = $true)][object]$Value)
  $Value | ConvertTo-Json -Depth 8 -Compress
}

function Get-CodexxState {
  if (-not $StatePath -or -not (Test-Path -LiteralPath $StatePath -PathType Leaf)) {
    throw 'Codex-X skin state file is missing.'
  }
  return Get-Content -LiteralPath $StatePath -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
}

function Get-CodexxDebugIdentity {
  param([Parameter(Mandatory = $true)][object]$Codex)
  foreach ($process in @(Get-DreamSkinCodexProcesses -Codex $Codex)) {
    $commandLine = "$($process.CommandLine)"
    foreach ($match in [regex]::Matches($commandLine, '(?i)(?:^|\s)--remote-debugging-port(?:=|\s+)(?<port>[0-9]{4,5})(?=$|\s)')) {
      $candidate = [int]$match.Groups['port'].Value
      if ($candidate -lt 1024 -or $candidate -gt 65535) { continue }
      $identity = Get-DreamSkinVerifiedCdpIdentity -Port $candidate -Codex $Codex
      if ($null -ne $identity) {
        return [pscustomobject]@{ Port = $candidate; Identity = $identity }
      }
    }
  }
  return $null
}

function Test-CodexxInjectorState {
  param([Parameter(Mandatory = $true)][object]$State)
  if (-not $State.injectorPid -or -not $State.injectorStartedAt -or
    -not $State.injectorPath -or -not $State.nodePath -or -not $State.port) {
    return $false
  }
  $processId = [int]$State.injectorPid
  $process = Get-CimInstance Win32_Process -Filter "ProcessId = $processId" -ErrorAction SilentlyContinue
  if ($null -eq $process) { return $false }
  $processPath = Get-DreamSkinProcessExecutablePath -ProcessInfo $process
  $commandLine = "$($process.CommandLine)"
  $startedAt = Get-DreamSkinProcessStartedAt -ProcessId $processId
  $portPattern = '(?i)(?:^|\s)--port(?:=|\s+)' + [regex]::Escape("$($State.port)") + '(?=$|\s)'
  return [bool](
    $processPath -and
    ([System.IO.Path]::GetFileName($processPath) -ieq 'node.exe') -and
    (Test-DreamSkinPathEqual -Left $processPath -Right "$($State.nodePath)") -and
    (Test-DreamSkinCommandLineToken -CommandLine $commandLine -Token "$($State.injectorPath)") -and
    (Test-DreamSkinCommandLineToken -CommandLine $commandLine -Token '--watch') -and
    [regex]::IsMatch($commandLine, $portPattern) -and
    $startedAt -eq "$($State.injectorStartedAt)"
  )
}

$operationLock = Enter-DreamSkinOperationLock
try {
switch ($Action) {
  'inspect' {
    $codex = Get-DreamSkinCodexInstall
    $processes = @(Get-DreamSkinCodexProcesses -Codex $codex)
    $debug = Get-CodexxDebugIdentity -Codex $codex
    $watcherPids = @(Get-CimInstance Win32_Process -Filter "Name = 'node.exe'" -ErrorAction SilentlyContinue |
      Where-Object {
        $_.CommandLine -and
        [regex]::IsMatch("$($_.CommandLine)", '(?i)(?:^|\s)--watch(?=$|\s)') -and
        "$($_.CommandLine)".IndexOf('injector.mjs', [System.StringComparison]::OrdinalIgnoreCase) -ge 0
      } | ForEach-Object { [int]$_.ProcessId })
    Write-CodexxJson ([pscustomobject]@{
      packageRoot = "$($codex.PackageRoot)"
      executable = "$($codex.Executable)"
      version = "$($codex.Version)"
      packageFullName = "$($codex.PackageFullName)"
      packageFamilyName = "$($codex.PackageFamilyName)"
      appUserModelId = "$($codex.AppUserModelId)"
      running = $processes.Count -gt 0
      debugPort = if ($null -eq $debug) { $null } else { [int]$debug.Port }
      browserId = if ($null -eq $debug) { $null } else { "$($debug.Identity.BrowserId)" }
      watcherPids = $watcherPids
    })
    break
  }
  'selectPort' {
    Assert-DreamSkinPort -Port $Port
    Write-CodexxJson ([pscustomobject]@{ port = (Select-DreamSkinPort -PreferredPort $Port) })
    break
  }
  'launch' {
    Assert-DreamSkinPort -Port $Port
    $codex = Get-DreamSkinCodexInstall
    $preserved = @(Get-DreamSkinCodexProcesses -Codex $codex | ForEach-Object { [int]$_.ProcessId })
    $arguments = @('--remote-debugging-address=127.0.0.1', "--remote-debugging-port=$Port")
    try {
      $launch = Start-DreamSkinCodexForDebugging -Codex $codex -Arguments $arguments `
        -Port $Port -PreserveProcessIds $preserved
      $deadline = (Get-Date).AddSeconds(45)
      $identity = Get-DreamSkinVerifiedCdpIdentity -Port $Port -Codex $codex
      while ($null -eq $identity -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 350
        $identity = Get-DreamSkinVerifiedCdpIdentity -Port $Port -Codex $codex
      }
      if ($null -eq $identity) {
        throw "Codex did not expose a verified loopback CDP endpoint on port $Port within 45 seconds."
      }
      Write-CodexxJson ([pscustomobject]@{
        port = $Port
        browserId = "$($identity.BrowserId)"
        strategy = "$($launch.Strategy)"
      })
    } catch {
      try { Stop-DreamSkinCodex -Codex $codex -PreserveProcessIds $preserved -AllowForce } catch {}
      if ($preserved.Count -eq 0 -and (Get-DreamSkinCodexProcesses -Codex $codex).Count -eq 0) {
        try { $null = Start-DreamSkinCodex -Codex $codex } catch {}
      }
      throw
    }
    break
  }
  'stopCodex' {
    $codex = Get-DreamSkinCodexInstall
    Stop-DreamSkinCodex -Codex $codex -AllowForce
    Write-CodexxJson ([pscustomobject]@{ stopped = $true })
    break
  }
  'launchNormal' {
    $codex = Get-DreamSkinCodexInstall
    $processId = Start-DreamSkinCodex -Codex $codex
    Write-CodexxJson ([pscustomobject]@{ processId = [int]$processId })
    break
  }
  'verifyPort' {
    Assert-DreamSkinPort -Port $Port
    $codex = Get-DreamSkinCodexInstall
    $identity = Get-DreamSkinVerifiedCdpIdentity -Port $Port -Codex $codex
    Write-CodexxJson ([pscustomobject]@{
      verified = $null -ne $identity
      browserId = if ($null -eq $identity) { $null } else { "$($identity.BrowserId)" }
    })
    break
  }
  'processInfo' {
    if ($TargetPid -le 0) { throw 'Target PID must be positive.' }
    $process = Get-CimInstance Win32_Process -Filter "ProcessId = $TargetPid" -ErrorAction SilentlyContinue
    if ($null -eq $process) {
      Write-CodexxJson ([pscustomobject]@{ alive = $false })
      break
    }
    Write-CodexxJson ([pscustomobject]@{
      alive = $true
      path = (Get-DreamSkinProcessExecutablePath -ProcessInfo $process)
      commandLine = "$($process.CommandLine)"
      startedAt = (Get-DreamSkinProcessStartedAt -ProcessId $TargetPid)
    })
    break
  }
  'injectorStatus' {
    $state = Get-CodexxState
    Write-CodexxJson ([pscustomobject]@{ active = (Test-CodexxInjectorState -State $state) })
    break
  }
  'stopInjector' {
    $state = Get-CodexxState
    if (-not (Test-CodexxInjectorState -State $state)) {
      $process = Get-Process -Id ([int]$state.injectorPid) -ErrorAction SilentlyContinue
      if ($null -ne $process) { throw 'The recorded skin injector identity does not match; it was not stopped.' }
    } else {
      $null = Stop-DreamSkinRecordedInjector -State $state
    }
    Write-CodexxJson ([pscustomobject]@{ stopped = $true })
    break
  }
}
} finally {
  Exit-DreamSkinOperationLock -Mutex $operationLock
}
