# NAT-PMP Mod for Proton VPN and qBittorrent

A Docker Mod for [linuxserver/docker-qbittorrent](https://github.com/linuxserver/docker-qbittorrent) that automatically configures qBittorrent's listening port using NAT-PMP through [Proton VPN](https://protonvpn.com/).

The mod requests TCP and UDP port mappings from the VPN gateway, updates qBittorrent's listening port, and periodically renews the mappings to keep the port accessible.

## Features

- Automatically obtains a public port through NAT-PMP.
- Configures both TCP and UDP mappings using the same external port.
- Automatically updates and verifies qBittorrent's listening port.
- Renews port mappings every 45 seconds, using a requested mapping lifetime of 60 seconds.
- Retries failed requests with exponential backoff.
- Supports optional qBittorrent WebUI authentication.
- Provides timestamped logs for requests, errors, retries, renewals, and port updates.

## Installation

Add the following environment variable to your qBittorrent container to enable the mod:

```yaml
DOCKER_MODS: "multinerd/docker-mods-qbittorrent-protonvpn-natpmp:latest"
```

### Docker Compose example

The following example shows the relevant qBittorrent configuration. It assumes you already have a configured Gluetun container connected to Proton VPN.

```yaml
services:
  qbittorrent:
    container_name: qbittorrent
    image: lscr.io/linuxserver/qbittorrent:latest
    restart: unless-stopped

    environment:
      PUID: "1000"
      PGID: "1000"
      TZ: America/New_York

      # qBittorrent WebUI port.
      WEBUI_PORT: "6969"

      # Enable the NAT-PMP Docker Mod.
      DOCKER_MODS: "multinerd/docker-mods-qbittorrent-protonvpn-natpmp:latest"

      # Optional: Set both variables if WebUI authentication is required.
      # QBITTORRENT_WEBUI_USERNAME: "your-username"
      # QBITTORRENT_WEBUI_PASSWORD: "your-password"

    # Share Gluetun's network namespace.
    network_mode: service:gluetun

    volumes:
      - ./data/qbittorrent/config:/config
      - ./data/qbittorrent/downloads:/downloads
```

Ensure that qBittorrent and Gluetun share the same network namespace so the mod can reach the VPN gateway and qBittorrent's local WebUI.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `DOCKER_MODS` | Not set | Enables this mod when configured with the image above. |
| `WEBUI_PORT` | `8080` | qBittorrent WebUI port. This is the only configurable part of the WebUI endpoint. |
| `QBITTORRENT_WEBUI_USERNAME` | Unset | Optional WebUI username. |
| `QBITTORRENT_WEBUI_PASSWORD` | Unset | Optional WebUI password. |

### Fixed settings

The following settings are hardcoded and cannot be changed through environment variables:

- **NAT-PMP gateway:** `10.2.0.1`
- **qBittorrent WebUI address:** `http://127.0.0.1:${WEBUI_PORT}`
- **Requested mapping lifetime:** 60 seconds
- **Mapping renewal interval:** 45 seconds

The WebUI endpoint defaults to `http://127.0.0.1:8080`. Set `WEBUI_PORT` to match your qBittorrent WebUI configuration.

## Authentication

By default, the mod accesses the qBittorrent WebUI through localhost. If you have enabled qBittorrent's **Bypass authentication for clients on localhost** setting, you can leave the username and password variables unset.

Otherwise, configure both `QBITTORRENT_WEBUI_USERNAME` and `QBITTORRENT_WEBUI_PASSWORD`. If authentication fails, verify the credentials and check whether the WebUI has banned the connecting IP.

## Logging and troubleshooting

The mod logs its activity, including:

- NAT-PMP gateway responses and mapping requests.
- Assigned TCP and UDP ports and reported mapping lifetimes.
- Mapping renewals and external port changes.
- qBittorrent authentication, listening-port updates, and verification.
- Request failures and retry delays.

View the container logs with:

```bash
docker logs -f qbittorrent
```

If port mapping fails, verify that:

1. qBittorrent shares Gluetun's network namespace.
2. The VPN connection is established and the NAT-PMP gateway is reachable at `10.2.0.1`.
3. `WEBUI_PORT` matches the qBittorrent WebUI port.
4. The WebUI authentication settings and credentials are correct.

Renewal attempts can be delayed by network errors or NAT-PMP request timeouts. The 45-second interval is the intended renewal schedule, not a guarantee of successful renewal under all network conditions.

## License

Licensed under the [MIT License](LICENSE).
