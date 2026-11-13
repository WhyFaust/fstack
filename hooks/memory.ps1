param([string] $EventName, [string] $EncodedWorkingDirectory)
$ErrorActionPreference = 'Stop'
if (!$env:PLUGIN_ROOT) { exit 0 }
$binary = Join-Path $env:PLUGIN_ROOT 'bin\fstack.exe'
$item = Get-Item -LiteralPath $binary -Force -ErrorAction SilentlyContinue
if (!$item -or $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { exit 0 }
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = $item.FullName
$start.WorkingDirectory = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($EncodedWorkingDirectory))
$start.UseShellExecute = $false
$start.Arguments = 'memory hook ' + $EventName
$process = [Diagnostics.Process]::Start($start)
try {
    $process.WaitForExit()
    exit $process.ExitCode
} finally {
    $process.Dispose()
}
