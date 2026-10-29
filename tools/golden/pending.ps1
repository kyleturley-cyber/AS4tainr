# Capture every list in tools\golden\queue.txt that has not been
# captured yet, one after another (golden.ps1 for each): one command for
# all the captures waiting, instead of one per list.
#
# Usage (Windows PowerShell, from the repository):
#   powershell -ExecutionPolicy Bypass -File tools\golden\pending.ps1
#   powershell -ExecutionPolicy Bypass -File tools\golden\pending.ps1 -DryRun
#
# queue.txt names one list per line ("verify23", or "verify23|lst" when
# its files are in another folder than tools\golden\verify23); '#' lines
# are comments. A list is pending when one of its commands has no
# target\ibmi\golden\<name>.txt yet. Lists whose files folder exists get
# -Files <folder>. -DryRun shows what would run.
param(
  [string]$Queue = (Join-Path $PSScriptRoot 'queue.txt'),
  [string]$Out   = (Join-Path $PSScriptRoot '..\..\target\ibmi\golden'),
  [switch]$DryRun
)
$entries = Get-Content $Queue | Where-Object { $_ -match '\S' -and $_ -notmatch '^\s*#' }
$pending = @()
foreach ($e in $entries) {
  $list, $folder = ($e.Trim() -split '\|', 2)
  if (-not $folder) { $folder = $list }
  $path = Join-Path $PSScriptRoot "$list.txt"
  if (-not (Test-Path $path)) { Write-Warning "no list $path"; continue }
  $names = Get-Content $path | Where-Object { $_ -match '\S' -and $_ -notmatch '^\s*#' } |
    ForEach-Object { ($_ -split '\|', 2)[0].Trim() }
  $missing = @($names | Where-Object { -not (Test-Path (Join-Path $Out "$_.txt")) })
  if ($missing.Count -eq 0) { Write-Host "captured already: $list"; continue }
  $pending += [pscustomobject]@{ List = $path; Folder = (Join-Path $PSScriptRoot $folder); Name = $list; Missing = $missing.Count }
}
if ($pending.Count -eq 0) { Write-Host 'nothing to capture'; exit 0 }
foreach ($p in $pending) {
  Write-Host "== $($p.Name): $($p.Missing) commands not captured"
  if ($DryRun) { continue }
  $gargs = @{ List = $p.List }
  if (Test-Path $p.Folder -PathType Container) { $gargs.Files = $p.Folder }
  & (Join-Path $PSScriptRoot 'golden.ps1') @gargs
  if ($LASTEXITCODE -ne 0) {
    Write-Warning "$($p.Name) did not complete (PUB400 refused or timed out); run pending.ps1 again later"
  }
}
