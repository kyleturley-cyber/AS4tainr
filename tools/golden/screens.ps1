# Capture 5250 screens from PUB400 (a real IBM i) for comparison with
# as4tainr's screens. See docs/golden-outputs.md.
#
# Usage (Windows PowerShell, from the repository):
#   powershell -ExecutionPolicy Bypass -File tools\golden\screens.ps1 [-List tools\golden\screens.txt]
#
# It opens an SSH tunnel to PUB400's 5250 port (ssh -L), signs on with the
# user name given and the password you type (it is sent to PUB400 only,
# never written anywhere), runs the steps of the list and writes each
# screen as text to target\ibmi\screens\<name>.txt. Text in non-display
# fields (the password) is never part of a screen written.
#
# A list has one "name|actions" per line: text to type and keys in braces:
# {ENTER} {F1}-{F24} {PAGEUP} {PAGEDOWN} {TAB} {FIELDEXIT} {ERASEEOF}
# {WAIT n} {USER} {PASSWORD}. After a key that goes to the host, the
# screen is written when the host has answered.
param(
  [string]$List = (Join-Path $PSScriptRoot 'screens.txt'),
  [string]$Out  = (Join-Path $PSScriptRoot '..\..\target\ibmi\screens'),
  [string]$HostAlias = 'pub400',
  [string]$User = 'AS4TRAINR',
  [int]$Port = 50023,
  [int]$QuietMs = 1500
)
$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'Tn5250Capture.cs')
$secure = Read-Host "Password for $User on $HostAlias" -AsSecureString
$ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
$tunnel = $null
try {
  $password = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
  Write-Host "opening the tunnel (local port $Port to $HostAlias port 23)"
  $tunnel = Start-Process ssh -ArgumentList @('-N', '-o', 'ExitOnForwardFailure=yes', '-L', "${Port}:localhost:23", $HostAlias) -PassThru -WindowStyle Hidden
  $ok = $false
  for ($t = 0; $t -lt 30; $t++) {
    Start-Sleep 1
    try { $c = New-Object Net.Sockets.TcpClient('127.0.0.1', $Port); $c.Close(); $ok = $true; break } catch { }
    if ($tunnel.HasExited) { break }
  }
  if (-not $ok) { throw "the tunnel did not open (ssh exited: $($tunnel.HasExited)); try again" }
  $out = [IO.Path]::GetFullPath($Out)
  $n = [Tn5250Capture]::RunList('127.0.0.1', $Port, (Resolve-Path $List).Path, $out, $User, $password, $QuietMs)
  Write-Host "$n screens written to $out"
} finally {
  [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
  $password = $null
  if ($tunnel -and -not $tunnel.HasExited) { Stop-Process -Id $tunnel.Id -Force }
}
