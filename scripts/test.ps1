param(
    [string]$Toolchain = ""
)

$ErrorActionPreference = "Stop"

$cargoArgs = @("test")
if ($Toolchain -ne "") {
    $cargoArgs = @($Toolchain) + $cargoArgs
}

& cargo @cargoArgs
