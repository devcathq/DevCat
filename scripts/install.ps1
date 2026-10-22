# Install DevCat from PowerShell (Windows):
#
#   irm https://devcathq.github.io/DevCat/install.ps1 | iex                                   # the CLI (devcat serve)
#   & ([scriptblock]::Create((irm https://devcathq.github.io/DevCat/install.ps1))) -App       # the desktop app
#
# The CLI goes to %LOCALAPPDATA%\Programs\DevCat CLI and onto your user PATH (no admin needed).
param(
    [switch]$App,
    [switch]$Both,
    [string]$Version = ""
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$repo = "devcathq/DevCat"
if ($Version) {
    if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
    $base = "https://github.com/$repo/releases/download/$Version"
} else {
    $base = "https://github.com/$repo/releases/latest/download"
}
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("devcat-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null

try {
    if (-not $App -or $Both) {
        $file = "devcat-cli-x86_64-pc-windows-msvc.zip"
        Write-Host "Downloading $file..."
        Invoke-WebRequest "$base/$file" -OutFile "$tmp\$file"
        Invoke-WebRequest "$base/$file.sha256" -OutFile "$tmp\$file.sha256"
        $want = (Get-Content "$tmp\$file.sha256" -Raw).Split(" ")[0].Trim().ToLower()
        $got = (Get-FileHash "$tmp\$file" -Algorithm SHA256).Hash.ToLower()
        if ($want -ne $got) { throw "checksum mismatch for $file" }
        $dir = Join-Path $env:LOCALAPPDATA "Programs\DevCat CLI"
        New-Item -ItemType Directory -Force $dir | Out-Null
        Expand-Archive "$tmp\$file" -DestinationPath $dir -Force
        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if (-not ($userPath -split ";" | Where-Object { $_ -eq $dir })) {
            [Environment]::SetEnvironmentVariable("Path", ($userPath.TrimEnd(";") + ";" + $dir).TrimStart(";"), "User")
            $env:Path += ";$dir"
            Write-Host "Added $dir to your PATH (new terminals pick it up)."
        }
        & "$dir\devcat.exe" version
        Write-Host "Installed to $dir\devcat.exe. Start it with: devcat serve   (devcat help for more)"
    }
    if ($App -or $Both) {
        $latest = Invoke-RestMethod "$base/latest.json"
        $v = $latest.version.TrimStart("v")
        $file = "DevCat_${v}_x64-setup.exe"
        Write-Host "Downloading $file..."
        Invoke-WebRequest "$base/$file" -OutFile "$tmp\$file"
        Write-Host "Running the installer..."
        Start-Process "$tmp\$file" -Wait
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
