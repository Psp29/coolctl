# Maintainer: local build only, not published to the AUR
pkgname=coolctl
pkgver=0.1.0
pkgrel=1
pkgdesc="Daemon and GTK4 GUI for DeepCool AIO pump LCDs (LM360 and compatible)"
arch=('x86_64')
license=('custom')
depends=('gtk4' 'gstreamer' 'gst-plugins-base' 'gst-plugins-good' 'gst-plugins-bad' 'libusb')
optdepends=('gst-libav: additional video codec support for less common formats')
makedepends=('rust' 'cargo')

# No source array: this builds straight from the local working tree ($startdir),
# not a downloaded/VCS source — run `makepkg` from this directory.
source=()
sha256sums=()

build() {
    cd "$startdir"
    cargo build --release --workspace
}

package() {
    cd "$startdir"
    install -Dm755 target/release/coolctld "$pkgdir/usr/bin/coolctld"
    install -Dm755 target/release/coolctl-gui "$pkgdir/usr/bin/coolctl-gui"
    install -Dm644 systemd/coolctl.service "$pkgdir/usr/lib/systemd/system/coolctl.service"
    install -Dm644 packaging/coolctl-gui.desktop "$pkgdir/usr/share/applications/coolctl-gui.desktop"
}
