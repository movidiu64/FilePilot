# Simulates a slow download with a long pause in the middle (a dropped connection), to check
# that FilePilot does not move the file before it is finished. The file stays open the whole
# time, like in a download manager.
#
#   powershell -ExecutionPolicy Bypass -File tools\slow-download.ps1 -Folder "$env:USERPROFILE\Downloads"
#
# Test rule: extension is "zip", move to another folder. Correct result: slow-test.zip shows up
# in the destination once, 12 MB, after the "done" message.
param(
    [string]$Folder = "$env:USERPROFILE\Downloads",
    [int]$PauseSeconds = 20
)
$path = Join-Path $Folder "slow-test.zip"
$chunk = New-Object byte[] (256KB)
$f = [System.IO.File]::Open($path, 'Create', 'Write', 'Read')
try {
    Write-Host "writing 6 MB in 6 s..."
    1..24 | ForEach-Object { $f.Write($chunk, 0, $chunk.Length); $f.Flush(); Start-Sleep -Milliseconds 250 }
    Write-Host "pausing $PauseSeconds s (the file stays open)..."
    Start-Sleep -Seconds $PauseSeconds
    Write-Host "6 MB more..."
    1..24 | ForEach-Object { $f.Write($chunk, 0, $chunk.Length); $f.Flush(); Start-Sleep -Milliseconds 250 }
} finally {
    $f.Close()
}
Write-Host "done: $((Get-Item $path -ErrorAction SilentlyContinue).Length) bytes at $path (if missing, it was already moved after closing)"
