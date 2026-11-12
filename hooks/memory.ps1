param([string] $EventName, [string] $EncodedWorkingDirectory)
$ErrorActionPreference = 'Stop'
if (!$env:PLUGIN_ROOT) { exit 0 }
$binary = Join-Path $env:PLUGIN_ROOT 'bin\fstack.exe'
$item = Get-Item -LiteralPath $binary -Force -ErrorAction SilentlyContinue
if (!$item -or $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { exit 0 }
# Use native inherited handles. Windows PowerShell's text pipeline can recode
# UTF-8 forwarded by Codex. The executable discovers the package from its path.
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = $item.FullName
# Capture the outer shell's OS directory before PowerShell resolves a literal
# path with wildcard characters as its provider's location.
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
