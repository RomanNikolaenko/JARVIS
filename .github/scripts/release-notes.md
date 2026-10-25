## Which file do I need?

| Your computer | File |
| --- | --- |
| Windows 10/11 on Intel or AMD (most PCs) | `JARVIS_{version}_x64-setup.exe` |
| Windows 11 on ARM (Snapdragon, Surface Pro X) | `JARVIS_{version}_arm64-setup.exe` |
| Windows, installs managed by an administrator | the `.msi` of the same processor type |
| Debian, Ubuntu, Mint (Intel or AMD) | the file ending in `_amd64.deb` |
| Fedora, openSUSE (Intel or AMD) | the file ending in `.x86_64.rpm` |
| Any other Linux (Intel or AMD) | the file ending in `_amd64.AppImage` |
| Linux on ARM64 | the files ending in `_arm64.deb`, `.aarch64.rpm` or `_aarch64.AppImage` |

Not sure? Windows: Settings → System → About → System type ("x64-based
processor" means x64). Linux: `uname -m` (`x86_64` or `aarch64`). An
installer for the wrong processor refuses to install.

The installers are not Authenticode-signed yet, so Windows SmartScreen warns
on the first start (More info → Run anyway). Check a download against
`SHA256SUMS.txt`. Installed copies find new versions themselves and install
them after you click the update button (signed updates).
