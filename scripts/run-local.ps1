param(
    [string]$ConfigPath = "configs/example.json",
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$cargoArgs = @("run")
if ($Release) {
    $cargoArgs += "--release"
}
$cargoArgs += "--"
$cargoArgs += $ConfigPath

& cargo @cargoArgs
