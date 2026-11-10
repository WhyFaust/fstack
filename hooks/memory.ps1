param([string] $EventName)
$ErrorActionPreference = 'Stop'
if (!$env:PLUGIN_ROOT) { exit 0 }
$binary = Join-Path $env:PLUGIN_ROOT 'bin\fstack.exe'
$item = Get-Item -LiteralPath $binary -Force -ErrorAction SilentlyContinue
if (!$item -or $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { exit 0 }
& $binary --package $env:PLUGIN_ROOT memory hook $EventName
exit $LASTEXITCODE
