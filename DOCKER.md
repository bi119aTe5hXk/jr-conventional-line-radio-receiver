# Docker and CasaOS

This container accesses a USB SDR directly on a Debian host and serves the receiver UI on port `8080` for desktop and mobile browsers. The web backend reuses the desktop application's `receiver-core` and SoapySDR input. It runs one receiver session at a time; for this initial version, use only one browser tab for audio playback.

## Start the container

```sh
docker compose up -d --build
docker compose logs -f jr-receiver
```

Open `http://<debian-or-casaos-host>:8080/`. Settings are stored in `./data/config`, and recordings are stored in `./data/recordings`.

The Compose configuration mounts `/dev/bus/usb` and grants access to USB character devices with major number `189`. Devices therefore remain accessible after reconnecting them or when their USB device number changes, without enabling `privileged: true`.

Do not run `rtl_tcp`, SDR++, SDRconnect, or another program that opens the same USB device at the same time.

## RTL-SDR

The image includes Debian's SoapyRTLSDR module. After connecting the receiver, check it from inside the container:

```sh
docker compose exec jr-receiver lsusb
docker compose exec jr-receiver SoapySDRUtil --find="driver=rtlsdr"
```

In the web UI, select `SoapySDR デバイス` and run the device scan, or enter the following device arguments manually:

```text
driver=rtlsdr
```

The default rate of `240000 S/s` is suitable for RTL-SDR. If the device or driver rejects it, use `1024000 S/s`.

If `lsusb` detects the RTL-SDR but SoapySDR cannot open it, check whether the Debian host loaded the `dvb_usb_rtl28xxu` DVB kernel driver. If it owns the device, unload or blacklist that driver on the host and reconnect the receiver. This is separate from container permissions.

## RSP1A and third-party OEM receivers

The receiver code does not depend on a particular SDR manufacturer. If a device can be enumerated by the container's SoapySDR 0.8 installation and supplies a CF32 IQ stream, the receiver DSP does not need device-specific changes.

The base image cannot automatically include SDRplay's proprietary API. A standard RSP1A normally requires:

1. SDRplay API 3.
2. A SoapySDRPlay3 module built for SoapySDR 0.8.
3. Access to both the USB device and the SDRplay API service from the container.

Whether a third-party OEM receiver works with `driver=sdrplay` depends on whether SDRplay API recognizes it. Some OEM receivers instead expose a Mirics-compatible interface and may require SoapyMiri. Use the actual `SoapySDRUtil --find` result to determine the correct driver.

Common lower output rates supported by SoapySDRPlay3 include `192000`, `250000`, and `384000`. Start with:

```text
device arguments: driver=sdrplay
sample rate: 250000
```

Validate the driver inside an image that has SDRplay API and SoapySDRPlay3 installed:

```sh
SoapySDRUtil --info
SoapySDRUtil --find
SoapySDRUtil --probe="driver=sdrplay"
```

If `--find` cannot see the device, resolve the USB, API, or SoapySDR module problem first. Changing the receiver DSP will normally not help at that stage.

## Current web-version limitations

- A browser cannot display the container's native file picker. IQ WAV and external preset paths must currently be entered as paths inside the container.
- Status and decoded audio use HTTP polling at approximately 180 ms. Multiple browsers can inspect status, but they compete for the current audio queue; enable audio in only one browser tab.
- Authentication is not implemented. Use the service only on a trusted LAN and do not expose port `8080` directly to the internet.
