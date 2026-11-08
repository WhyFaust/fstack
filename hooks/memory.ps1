param([string] $EventName)

function Test-TrustedFile([string] $Path) {
    if ($Path -notmatch '^[A-Za-z]:[\\/]') { return $false }
    $fullPath = [IO.Path]::GetFullPath($Path)
    $workspace = $PWD.ProviderPath.TrimEnd('\', '/') + '\'
    if ($fullPath -ne $Path -or $fullPath.StartsWith($workspace, [StringComparison]::OrdinalIgnoreCase)) {
        return $false
    }
    $item = Get-Item -LiteralPath $fullPath -Force -ErrorAction SilentlyContinue
    if (!$item -or $item.PSIsContainer) { return $false }
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $false }
    for ($directory = $item.Directory; $directory; $directory = $directory.Parent) {
        if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            (Get-Item -LiteralPath (Join-Path $directory.FullName '.git') -Force -ErrorAction SilentlyContinue)) {
            return $false
        }
    }
    return $true
}

$profileRoot = $env:CODEX_HOME
if (!$profileRoot -and $env:USERPROFILE) { $profileRoot = Join-Path $env:USERPROFILE '.codex' }
if (!$profileRoot) { exit 0 }

$pin = Join-Path $profileRoot 'fstack\memory\python-executable'
if (!(Test-TrustedFile $pin)) { exit 0 }
$python = [IO.File]::ReadAllText($pin).TrimEnd("`r", "`n")
if ($python -notlike '*.exe' -or !(Test-TrustedFile $python)) { exit 0 }
& $python -E -s -B "$env:PLUGIN_ROOT\skills\use-memory\scripts\memory.py" hook $EventName
exit $LASTEXITCODE
