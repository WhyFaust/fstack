param([string] $EventName, [string] $EncodedWorkingDirectory)
$ErrorActionPreference = 'Stop'
if (!$env:PLUGIN_ROOT) { exit 0 }
$launcher = [IO.Path]::Combine($env:PLUGIN_ROOT, 'launchers', 'fstack.ps1')
if ($EventName -eq 'Bootstrap') {
    $null = & ([scriptblock]::Create([IO.File]::ReadAllText($launcher))) -InstallRuntime
    [Console]::WriteLine('{}')
    exit 0
}
try {
    $launch = [scriptblock]::Create([IO.File]::ReadAllText($launcher))
    $wait = if ($EventName -eq 'SessionStart') { [Diagnostics.Stopwatch]::StartNew() }
    do {
        $binary = & $launch -RuntimePath
        if ($binary -or !$wait -or $wait.ElapsedMilliseconds -ge 6000) { break }
        Start-Sleep -Milliseconds 500
    } while ($true)
} catch { $binary = $null }
if (!$binary) {
    if ($EventName -eq 'UserPromptSubmit') {
        $file = [IO.Path]::Combine($env:PLUGIN_ROOT, 'hooks', 'runtime-unavailable.json')
        $output = [Console]::OpenStandardOutput()
        $bytes = [IO.File]::ReadAllBytes($file)
        $output.Write($bytes, 0, $bytes.Length)
    }
    exit 0
}
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = $binary
$start.WorkingDirectory = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($EncodedWorkingDirectory))
$start.UseShellExecute = $false
$package = [IO.Path]::GetFullPath($env:PLUGIN_ROOT).TrimEnd([char]92, [char]47)
$start.Arguments = '--package "' + $package + '" hook ' + $EventName
$process = [Diagnostics.Process]::Start($start)
try {
    $process.WaitForExit()
    exit $process.ExitCode
} finally {
    $process.Dispose()
}
