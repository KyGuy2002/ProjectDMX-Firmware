$ErrorActionPreference = "SilentlyContinue"

while ($true) {
    Write-Host "Waiting for RP2350..."

    probe-rs attach `
        --log-format "{s}" `
        --chip RP235x `
        "target/thumbv8m.main-none-eabihf/debug/pico_rust_template"

    Write-Host "Device disconnected."
    Start-Sleep -Seconds 1
}