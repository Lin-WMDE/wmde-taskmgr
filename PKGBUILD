# Maintainer: WMDE <https://wmde.fun>
#
# Builds our own Lin-WMDE/wmde-taskmgr (branch wmde). No upstream: written here and never
# merged from anywhere. COSMIC 1.9 ships a system monitor of its own, cosmic-monitor; the
# stack does not take it and keeps this one.
#
# The window binds to fun.wmde.Comp.Windows in the compositor to learn which process owns
# which window; without a new enough wmde-comp the process list still works, only the
# application grouping falls back to the process tree.
pkgname=wmde-taskmgr
pkgver=0.1.0
pkgrel=1
pkgdesc="WMDE task manager - processes, load and users"
arch=('x86_64')
url="https://wmde.fun"
license=('GPL-3.0-only')
# runtime: wayland client (libcosmic winit/wayland). Vulkan is reached through dlopen,
# so makepkg cannot see it in ldd - the loader and a driver come from wmde-desktop.
depends=('glibc' 'gcc-libs' 'wayland')
optdepends=('wmde-icons-base: WMDE icon theme')
makedepends=('rust' 'cargo' 'just' 'git' 'wayland' 'clang' 'lld' 'pkgconf')
source=("$pkgname::git+https://github.com/Lin-WMDE/wmde-taskmgr.git#branch=wmde")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$pkgname"
  # WMDE version: <COSMIC major.minor>.<WMDE release>.<commits since the nearest tag>.g<short>.
  local desc
  desc=$(git describe --long --tags --abbrev=7 2>/dev/null || true)
  if [ -n "$desc" ]; then
    printf '1.11.1.%s.g%s' "$(printf '%s' "$desc" | sed -E 's/.*-([0-9]+)-g[0-9a-f]+$/\1/')" "$(git rev-parse --short=7 HEAD)"
  else
    printf '1.11.1.%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
  fi
}

build() {
  cd "$srcdir/$pkgname"
  # x86-64-v3 (AVX2/BMI2) baseline for the WMDE repo; runs on Haswell+ (and the VM).
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=x86-64-v3"
  just build-release
}

check() {
  cd "$srcdir/$pkgname"
  # The /proc parsing and the grouping are the whole of the risk here and need no display.
  just test
}

package() {
  cd "$srcdir/$pkgname"
  # installs /usr/bin/wmde-taskmgr, the desktop entry, the hicolor icon, and the SMBIOS
  # collector with its unit. The unit ships disabled, the Arch way: without it the memory
  # page shows what the memory is doing and nothing about the modules it is made of.
  just rootdir="$pkgdir" prefix=/usr install
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
