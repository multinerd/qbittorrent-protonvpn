# NATPMP Mod for ProtonVPN and qBittorrent

A Docker mod for [linuxserver/docker-qbittorrent](https://github.com/linuxserver/docker-qbittorrent)
to allow qBittorrent to liston on a port via [Proton VPN](https://protonvpn.com/).

## How to use

Just define these environnement variables to your qBittorrent container: 

* `DOCKER_MODS=multinerd/docker-mods-qbittorrent-protonvpn-natpmp:latest:main` to enable this mod

## License

Licensed under MIT license.
