# rpi-health-mqtt

`rpi-health-mqtt` is a lightweight service for publishing Raspberry Pi health
metrics to a local MQTT broker. It targets 32-bit ARMv7 Raspberry Pi OS and is
designed to integrate with Home Assistant through MQTT Device Discovery.

The project is under active development. Version 0.1.0 will include system
metrics, power and throttling diagnostics, resilient MQTT publishing, a
hardened systemd service, and installation tooling.

## Development validation

Every change is validated in Docker on Linux amd64 and for
`armv7-unknown-linux-gnueabihf` under QEMU:

```powershell
pwsh -NoProfile -File scripts/validate-delivery.ps1
```

The POSIX equivalent is:

```sh
./scripts/validate-delivery.sh
```

## License

This project is licensed under the [MIT License](LICENSE).
