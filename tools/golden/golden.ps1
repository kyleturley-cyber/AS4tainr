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
  [int]$Tries = 6,
  # Seconds one command may run before it is killed (a command waiting
  # for a reply to an inquiry message waits forever in this job).
  [int]$Timeout = 120
)
[Console]::OutputEncoding = [Text.Encoding]::UTF8
New-Item -ItemType Directory -Force $Out | Out-Null
$logs = Split-Path $Out -Parent
$items = Get-Content $List | Where-Object { $_ -match '\S' -and $_ -notmatch '^\s*#' } | ForEach-Object {
  $n, $c = $_ -split '\|', 2
  [pscustomobject]@{ Name = $n.Trim(); Cmd = $c.Trim() }
}
# One remote sh script for all commands: PUB400 drops many new
# connections. Each command gets an empty stdin (`system` would read the
# rest of the script) and runs in the background under a watchdog that
# kills it after $Timeout seconds ("TIMEOUT after N s" in its output).
$sh = foreach ($i in $items) {
  "echo '=====BEGIN $($i.Name)'"
  if ($i.Cmd.StartsWith('!')) {
    "( $($i.Cmd.Substring(1)) ) </dev/null >/tmp/golden_out.`$`$ 2>/tmp/golden_err.`$`$ &"
  } else {
    $q = $i.Cmd -replace "'", "'\''"
    "( system '$q' ) </dev/null >/tmp/golden_out.`$`$ 2>&1 &"
  }
  "p=`$!; rm -f /tmp/golden_to.`$`$; ( sleep $Timeout; kill -9 `$p && echo x >/tmp/golden_to.`$`$ ) </dev/null >/dev/null 2>&1 &"
  "w=`$!; wait `$p; rc=`$?; kill `$w 2>/dev/null"
  "cat /tmp/golden_out.`$`$; [ -f /tmp/golden_to.`$`$ ] && echo 'TIMEOUT after $Timeout s'; echo `"rc=`$rc`""
  if ($i.Cmd.StartsWith('!')) { "sed 's/^/2> /' /tmp/golden_err.`$`$" }
  "rm -f /tmp/golden_out.`$`$ /tmp/golden_err.`$`$ /tmp/golden_to.`$`$"
  "echo '=====END $($i.Name)'"
}
$sh = (($sh + "echo '=====DONE'") -join "`n") + "`n"
# The script goes over as a file and runs there: piping a long script into
# ssh deadlocks (Windows PowerShell writes all of a command's input before
# it reads any of its output, and ssh waits for its output to be read).
$local = Join-Path $env:TEMP 'golden.sh'
[IO.File]::WriteAllText($local, $sh, (New-Object Text.UTF8Encoding $false))
$remote = '/tmp/golden_' + [guid]::NewGuid().ToString('N').Substring(0, 8) + '.sh'
for ($t = 1; $t -le $Tries; $t++) {
  scp -q -o ConnectTimeout=20 $local "${HostAlias}:$remote" 2>$null
  if ($LASTEXITCODE -eq 0) { break }
  Write-Host "copying the script failed (try $t); retrying in 10 s"; Start-Sleep 10
}
if ($LASTEXITCODE -ne 0) { Write-Error 'could not copy the script to the IBM i'; exit 1 }
Write-Host "running $($items.Count) commands"
for ($t = 1; $t -le $Tries; $t++) {
  $log = Join-Path $logs "attempt$t"
  # Lines are written to attempt<N>.out as they arrive (UTF-8), so a run
  # can be followed while it goes.
  $w = [IO.StreamWriter]::new("$log.out", $false, [Text.UTF8Encoding]::new($false))
  $w.AutoFlush = $true
  $res = ssh -o ConnectTimeout=20 -o ServerAliveInterval=30 $HostAlias "sh $remote" 2> "$log.err" | ForEach-Object {
    $w.WriteLine($_)
    if ($_ -like '=====BEGIN *') { Write-Host "running $($_.Substring(11))" }
    elseif ($_ -like 'TIMEOUT after*') { Write-Host "  $_" }
    $_
  }
  $w.Close()
  "exit=$LASTEXITCODE" | Add-Content "$log.err"
  if ($res -contains '=====DONE') { break }
  Write-Host "attempt $t incomplete (see $log.out/.err); retrying in 15 s"
  Start-Sleep 15
}
ssh -o ConnectTimeout=20 $HostAlias "rm -f $remote" 2>$null
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
