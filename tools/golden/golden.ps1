# Capture golden outputs from a real IBM i (PUB400) over SSH, for
# tools/golden_compare.py. See docs/golden-outputs.md.
#
# Usage (Windows PowerShell, from the repository):
#   powershell -ExecutionPolicy Bypass -File tools\golden\golden.ps1 [-List tools\golden\basics.txt]
#
# A list has one "name|CL command" per line (run with PASE `system`,
# stdout and stderr together), or "name|!shell line" (stderr lines
# prefixed "2> "); '#' lines are comments. Each result goes to
# target\ibmi\golden\<name>.txt, then "rc=N". The outputs hold IBM's
# message texts: they stay in target\ (not committed).
param(
  [string]$List = (Join-Path $PSScriptRoot 'basics.txt'),
  [string]$Out  = (Join-Path $PSScriptRoot '..\..\target\ibmi\golden'),
  [string]$HostAlias = 'pub400',
  [int]$Tries = 6
)
[Console]::OutputEncoding = [Text.Encoding]::UTF8
New-Item -ItemType Directory -Force $Out | Out-Null
$logs = Split-Path $Out -Parent
$items = Get-Content $List | Where-Object { $_ -match '\S' -and $_ -notmatch '^\s*#' } | ForEach-Object {
  $n, $c = $_ -split '\|', 2
  [pscustomobject]@{ Name = $n.Trim(); Cmd = $c.Trim() }
}
# One remote sh script for all commands: PUB400 drops many new
# connections. `system` gets an empty stdin, or it reads the rest of the
# script.
$sh = foreach ($i in $items) {
  "echo '=====BEGIN $($i.Name)'"
  if ($i.Cmd.StartsWith('!')) {
    "{ $($i.Cmd.Substring(1)) ; } </dev/null 2>/tmp/golden_err.`$`$; echo `"rc=`$?`"; sed 's/^/2> /' /tmp/golden_err.`$`$; rm -f /tmp/golden_err.`$`$"
  } else {
    $q = $i.Cmd -replace "'", "'\''"
    "system '$q' </dev/null 2>&1; echo `"rc=`$?`""
  }
  "echo '=====END $($i.Name)'"
}
$sh = ($sh + "echo '=====DONE'") -join "`n"
for ($t = 1; $t -le $Tries; $t++) {
  $log = Join-Path $logs "attempt$t"
  $res = $sh | ssh -o ConnectTimeout=20 $HostAlias "tr -d '\r' | sh" 2> "$log.err"
  $res | Set-Content "$log.out"
  "exit=$LASTEXITCODE" | Add-Content "$log.err"
  if ($res -contains '=====DONE') { break }
  Write-Host "attempt $t incomplete (see $log.out/.err); retrying in 15 s"
  Start-Sleep 15
}
if ($res -notcontains '=====DONE') { Write-Error 'no complete run'; exit 1 }
$name = $null; $buf = @()
foreach ($line in $res) {
  if ($line -like '=====BEGIN *') { $name = $line.Substring(11); $buf = @() }
  elseif ($line -like '=====END *') {
    $f = Join-Path $Out "$name.txt"
    [IO.File]::WriteAllLines($f, [string[]]$buf, (New-Object Text.UTF8Encoding $false))
    Write-Host "saved $name"; $name = $null
  }
  elseif ($name) { $buf += $line }
}
