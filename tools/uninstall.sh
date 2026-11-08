#!/usr/bin/env bash
# Gỡ mọi thứ đã cài lên host cho dự án android-pc.
#
#   tools/uninstall.sh            # chỉ gỡ gói pacman (mặc định)
#   tools/uninstall.sh --all      # gói pacman + toolchain + SDK + dữ liệu build
#   tools/uninstall.sh -n --all   # chỉ in ra sẽ làm gì, không xoá gì
#
# Tuỳ chọn:
#   --packages    gói pacman trong PACMAN_PKGS (kèm dependency không còn ai dùng)
#   --toolchains  rust toolchain nightly ghim trong rust-toolchain.toml, target musl
#   --sdk         Android SDK/NDK cài riêng cho apps/ trong ~/Android/Sdk
#   --data        aosp/ (gồm cả aosp/out), out/, target/ của dự án
#   --all         tất cả các mục trên
#   -n, --dry-run chỉ in lệnh
#   -y, --yes     không hỏi xác nhận
#
# Khi cài thêm gói host cho dự án, thêm tên gói vào PACMAN_PKGS bên dưới.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Gói được cài tường minh (pacman -S) cho dự án, ngày 2026-11-08.
# Dependency kéo theo (qemu-*, libburn, seabios, spice, ...) được pacman -Rns
# gỡ cùng nếu không còn gói nào khác cần.
PACMAN_PKGS=(
	qemu-desktop
	libisoburn # cung cấp xorriso
	edk2-ovmf
	limine
)

# Không bao giờ gỡ: đã có trên máy trước dự án.
#   rust stable, adb, waydroid, repo, git-lfs, clang, gcc, ~/.android,
#   /opt/android-sdk (gói android-ndk/android-platform; $ANDROID_SDK_ROOT trỏ vào đây)

DRY_RUN=0
ASSUME_YES=0
DO_PACKAGES=0
DO_TOOLCHAINS=0
DO_SDK=0
DO_DATA=0

for arg in "$@"; do
	case "$arg" in
	--packages) DO_PACKAGES=1 ;;
	--toolchains) DO_TOOLCHAINS=1 ;;
	--sdk) DO_SDK=1 ;;
	--data) DO_DATA=1 ;;
	--all) DO_PACKAGES=1 DO_TOOLCHAINS=1 DO_SDK=1 DO_DATA=1 ;;
	-n | --dry-run) DRY_RUN=1 ;;
	-y | --yes) ASSUME_YES=1 ;;
	-h | --help)
		sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "Tuỳ chọn không hợp lệ: $arg (xem --help)" >&2
		exit 2
		;;
	esac
done

if ((DO_PACKAGES + DO_TOOLCHAINS + DO_SDK + DO_DATA == 0)); then
	DO_PACKAGES=1
fi

run() {
	echo "+ $*"
	((DRY_RUN)) || "$@"
}

confirm() {
	((ASSUME_YES || DRY_RUN)) && return 0
	read -r -p "$1 [y/N] " reply
	[[ "$reply" == [yY]* ]]
}

remove_path() {
	local path="$1"
	[[ -e "$path" ]] || return 0
	local size
	size="$(du -sh "$path" 2>/dev/null | cut -f1)"
	if confirm "Xoá $path ($size)?"; then
		run rm -rf --one-file-system -- "$path"
	fi
}

if ((DO_PACKAGES)); then
	echo "== Gói pacman"
	installed=()
	for pkg in "${PACMAN_PKGS[@]}"; do
		pacman -Qq "$pkg" &>/dev/null && installed+=("$pkg")
	done
	if ((${#installed[@]} == 0)); then
		echo "Không còn gói nào cần gỡ."
	else
		# pacman tự hỏi xác nhận và liệt kê toàn bộ gói sẽ bị gỡ.
		run sudo pacman -Rns "${installed[@]}"
	fi
fi

if ((DO_TOOLCHAINS)); then
	echo "== Rust toolchain"
	mapfile -t channels < <(
		find "$ROOT" -name rust-toolchain.toml -not -path '*/target/*' -not -path "$ROOT/aosp/*" \
			-exec sed -nE 's/^channel *= *"(nightly[^"]*)".*/\1/p' {} + | sort -u
	)
	for ch in "${channels[@]}"; do
		if rustup toolchain list | grep -q "^$ch-"; then
			run rustup toolchain uninstall "$ch"
		fi
	done
	if rustup target list --installed --toolchain stable 2>/dev/null | grep -qx x86_64-unknown-linux-musl; then
		run rustup target remove --toolchain stable x86_64-unknown-linux-musl
	fi
fi

if ((DO_SDK)); then
	echo "== Android SDK/NDK"
	# Cố ý không dùng $ANDROID_SDK_ROOT: nó trỏ tới /opt/android-sdk có từ trước.
	remove_path "$HOME/Android/Sdk"
	# Chỉ xoá ~/Android nếu đã rỗng sau khi gỡ SDK.
	[[ -d "$HOME/Android" ]] && run rmdir --ignore-fail-on-non-empty "$HOME/Android"
fi

if ((DO_DATA)); then
	echo "== Dữ liệu build"
	if pgrep -f "$ROOT/aosp" >/dev/null; then
		echo "Có tiến trình đang chạy trong aosp/ (sync/build); dừng nó trước rồi chạy lại." >&2
		((DRY_RUN)) || exit 1
	fi
	remove_path "$ROOT/aosp"
	remove_path "$ROOT/out"
	remove_path "$ROOT/target"
fi

echo "Xong."
