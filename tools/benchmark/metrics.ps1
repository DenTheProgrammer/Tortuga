$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class BenchIo {
  [StructLayout(LayoutKind.Sequential)] public struct Counters {
    public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes;
  }
  [DllImport("kernel32.dll")] public static extern bool GetProcessIoCounters(IntPtr handle, out Counters counters);
}
'@
while ($taskLine = [Console]::ReadLine()) {
  try {
    $taskRequest = $taskLine | ConvertFrom-Json
    if ($taskRequest.archive) {
      $taskRoot = [IO.Path]::GetFullPath([string]$taskRequest.root).TrimEnd('\') + '\'
      $taskTarget = [IO.Path]::GetFullPath([string]$taskRequest.archive)
      if (-not $taskTarget.StartsWith($taskRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe archive target' }
      Add-Type -AssemblyName System.IO.Compression.FileSystem
      $taskZip = [IO.Compression.ZipFile]::Open($taskTarget,[IO.Compression.ZipArchiveMode]::Create)
      try {
        $taskFolders = @(Get-Item -LiteralPath $taskRoot) + @(Get-ChildItem -LiteralPath $taskRoot -Directory -Filter 'round-*')
        foreach ($taskFolder in $taskFolders) {
          foreach ($taskFile in (Get-ChildItem -LiteralPath $taskFolder.FullName -File)) {
            if ($taskFile.Name -notin @('report.json','report.txt','report.html','environment.json','torrent.json','failure.txt','samples.csv','samples.jsonl','peers.jsonl','settings.json','details.json','result.json','error.json','engine.log','stderr.log','diagnostics.log')) { continue }
            $taskEntry = $taskFile.FullName.Substring($taskRoot.Length).Replace('\','/')
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($taskZip,$taskFile.FullName,$taskEntry) | Out-Null
          }
        }
      } finally { $taskZip.Dispose() }
      $taskResult = @{archive=$taskTarget}
    } elseif ($taskRequest.versions) {
      $taskVersions = foreach($taskExe in $taskRequest.versions) {
        $taskVersion = (Get-Item -LiteralPath $taskExe).VersionInfo
        [pscustomobject]@{exe=$taskExe;version=$taskVersion.FileVersion;product=$taskVersion.ProductName}
      }
      $taskResult = @{versions=@($taskVersions)}
    } elseif ($taskRequest.stopHelpers) {
      $taskRoot = [IO.Path]::GetFullPath([string]$taskRequest.root).TrimEnd('\') + '\'
      $taskTarget = [IO.Path]::GetFullPath([string]$taskRequest.stopHelpers).TrimEnd('\') + '\'
      if (-not $taskTarget.StartsWith($taskRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe process scope' }
      $taskProcesses = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($taskTarget,[StringComparison]::OrdinalIgnoreCase) }
      foreach ($taskProcess in $taskProcesses) { Stop-Process -Id $taskProcess.ProcessId -ErrorAction SilentlyContinue }
      $taskResult = @{stopped=@($taskProcesses | Select-Object -ExpandProperty ProcessId)}
    } elseif ($taskRequest.cleanup) {
      $taskRoot = [IO.Path]::GetFullPath([string]$taskRequest.root).TrimEnd('\') + '\'
      $taskTarget = [IO.Path]::GetFullPath([string]$taskRequest.cleanup)
      if (-not $taskTarget.StartsWith($taskRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe cleanup target' }
      if ([IO.Directory]::Exists($taskTarget)) {
        foreach ($taskFile in [IO.Directory]::EnumerateFiles($taskTarget,'*',[IO.SearchOption]::AllDirectories)) {
          $taskResolved = [IO.Path]::GetFullPath($taskFile)
          if (-not $taskResolved.StartsWith($taskRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe cleanup file' }
          $taskAttributes = [IO.File]::GetAttributes($taskResolved)
          [IO.File]::SetAttributes($taskResolved,($taskAttributes -band (-bnot [IO.FileAttributes]::ReadOnly)))
        }
        [IO.Directory]::Delete($taskTarget,$true)
      }
      $taskResult = @{deleted=$true}
    } elseif ($taskRequest.network) {
      $taskRoutes = Get-NetRoute | Where-Object DestinationPrefix -in @('0.0.0.0/0','0.0.0.0/1','128.0.0.0/1','::/0','::/1','8000::/1') | Select-Object DestinationPrefix,InterfaceAlias,InterfaceIndex,NextHop,RouteMetric
      $taskDns = Get-DnsClientServerAddress | Where-Object { $_.ServerAddresses.Count -gt 0 } | Select-Object InterfaceAlias,ServerAddresses
      $taskRunning = Get-Process tortuga,utorrent -ErrorAction SilentlyContinue | Select-Object Id,Path
      $taskResult = @{routes=@($taskRoutes);dns=@($taskDns);running=@($taskRunning);vpnActive=[bool](@($taskRoutes | Where-Object InterfaceAlias -match '(?i)tun\d*|wireguard|amnezia|openvpn|tap|vpn|warp').Count)}
    } else {
      $taskProcess = Get-Process -Id ([int]$taskRequest.pid)
      $taskIo = New-Object BenchIo+Counters
      $taskOk = [BenchIo]::GetProcessIoCounters($taskProcess.Handle,[ref]$taskIo)
      $taskResult = @{cpuMs=$taskProcess.TotalProcessorTime.TotalMilliseconds;rssBytes=$taskProcess.WorkingSet64;ioReadBytes=$taskIo.ReadBytes;ioWriteBytes=$taskIo.WriteBytes;ioAvailable=$taskOk}
      if ($taskRequest.connections) {
        $taskConnections = Get-NetTCPConnection -OwningProcess $taskProcess.Id -ErrorAction SilentlyContinue | Where-Object State -eq 'Established' | Select-Object LocalAddress,RemoteAddress,RemotePort
        $taskResult.connections = @($taskConnections)
      }
    }
    [Console]::WriteLine(($taskResult | ConvertTo-Json -Depth 7 -Compress))
  } catch { [Console]::WriteLine((@{error=$_.Exception.Message} | ConvertTo-Json -Compress)) }
}
