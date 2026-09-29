# Молча ставит Windows-приложение, открывает его ссылку «добавить подписку» на
# capture_server.py и сохраняет данные, из которых считаются заголовки устройства
# (MachineGuid, имя компьютера, версия Windows), рядом с пойманными запросами.
#   pwsh .github/capture/windows.ps1 -Installer setup-Happ.x64.exe -Link 'happ://add/http://127.0.0.1:18080/sub/abc' -Out captures
param(
    [Parameter(Mandatory)] [string] $Installer,
    [Parameter(Mandatory)] [string] $Link,
    [string] $Out = "captures",
    [string] $ExeName = "Happ.exe"
)
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force $Out | Out-Null

function Find-App {
    $roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, "$env:LOCALAPPDATA\Programs", $env:LOCALAPPDATA, $env:APPDATA)
    foreach ($root in $roots) {
        if (-not $root -or -not (Test-Path $root)) { continue }
        $hit = Get-ChildItem $root -Recurse -Filter $ExeName -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -notmatch "unins|setup|update|crash" } | Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

function Save-Screen([string] $name) {
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
        $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
        $bmp.Save("$Out\screen-$name.png")
    } catch { "снимок экрана не удался: $_" | Out-File -Append "$Out\notes.txt" }
}

$crypto = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Cryptography"
$cv = Get-ItemProperty "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion"
@(
    "MachineGuid=$($crypto.MachineGuid)"
    "ComputerName=$env:COMPUTERNAME"
    "ProductName=$($cv.ProductName)"
    "DisplayVersion=$($cv.DisplayVersion)"
    "CurrentBuild=$($cv.CurrentBuild)"
    "UBR=$($cv.UBR)"
    "EditionID=$($cv.EditionID)"
    "Culture=$((Get-Culture).Name)"
    "UICulture=$((Get-UICulture).Name)"
    "Arch=$env:PROCESSOR_ARCHITECTURE"
    "OSVersion=$([Environment]::OSVersion.VersionString)"
    "ClockUtc=$([DateTime]::UtcNow.ToString('o'))"
) | Out-File -Encoding utf8 "$Out\device.txt"
Get-Content "$Out\device.txt"

# Ключи тихой установки разные у Inno Setup, NSIS и MSI: пробуем по очереди.
$app = $null
foreach ($switches in @("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /ALLUSERS", "/S", "/quiet /norestart")) {
    if ($app) { break }
    "установка с ключами $switches"
    $p = Start-Process $Installer -ArgumentList $switches -PassThru
    if (-not $p.WaitForExit(240000)) { $p | Stop-Process -Force }
    $app = Find-App
}
if (-not $app) { "приложение не найдено после установки"; exit 1 }
"приложение: $app"
(Get-Item $app).VersionInfo | Format-List | Out-File "$Out\version.txt"

# Ссылки открываются через обработчик протокола, найденный exe может быть вспомогательным.
Start-Process $Link
Start-Sleep 25
Save-Screen "protocol"
Get-Process | Where-Object { $_.Path -eq $app } | Stop-Process -Force
Get-ChildItem $Out
