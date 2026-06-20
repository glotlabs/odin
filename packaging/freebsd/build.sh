#!/bin/sh

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "${script_dir}/../.." && pwd)
work_dir="${script_dir}/work"
stage_dir="${work_dir}/fakeroot"
meta_dir="${work_dir}/metadata"
plist="${work_dir}/plist"
out_dir="${script_dir}/dist"
stage_only=0

case "${1:-}" in
	"")
		;;
	--stage-only)
		stage_only=1
		;;
	*)
		echo "usage: $0 [--stage-only]" >&2
		exit 64
		;;
esac

version=$(awk -F '"' '/^version = / { print $2; exit }' "${repo_root}/Cargo.toml")
package_name="odin-${version}"

if [ -z "${version}" ]; then
	echo "could not determine package version from Cargo.toml" >&2
	exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
	echo "cargo is required" >&2
	exit 1
fi

if [ "${stage_only}" -eq 0 ] && ! command -v pkg >/dev/null 2>&1; then
	echo "FreeBSD pkg is required" >&2
	exit 1
fi

rm -rf "${work_dir}" "${out_dir}"
mkdir -p "${stage_dir}" "${meta_dir}" "${out_dir}"

(cd "${repo_root}" && cargo build --release --locked)

install -d -m 0755 "${stage_dir}/opt/odin/bin"
install -d -m 0755 "${stage_dir}/opt/odin/etc/odin/services"
install -d -m 0755 "${stage_dir}/opt/odin/share/doc/odin"
install -d -m 0755 "${stage_dir}/opt/odin/share/examples/odin/services"
install -d -m 0755 "${stage_dir}/usr/local/bin"
install -d -m 0755 "${stage_dir}/usr/local/etc/rc.d"
install -d -m 0755 "${stage_dir}/usr/local/etc/newsyslog.conf.d"
install -d -m 0755 "${stage_dir}/var/db/odin"
install -d -m 0755 "${stage_dir}/var/log/odin"

install -m 0555 "${repo_root}/target/release/odin" "${stage_dir}/opt/odin/bin/odin"
ln -s /opt/odin/bin/odin "${stage_dir}/usr/local/bin/odin"
install -m 0644 "${repo_root}/README.md" "${stage_dir}/opt/odin/share/doc/odin/README.md"
install -m 0644 "${repo_root}/examples/services/hello.toml" "${stage_dir}/opt/odin/etc/odin/services/hello.toml.sample"
install -m 0644 "${repo_root}/examples/services/hello.toml" "${stage_dir}/opt/odin/share/examples/odin/services/hello.toml"
install -m 0644 "${repo_root}/examples/services/web.toml" "${stage_dir}/opt/odin/share/examples/odin/services/web.toml"
install -m 0644 "${script_dir}/files/newsyslog/odin.conf" "${stage_dir}/usr/local/etc/newsyslog.conf.d/odin.conf"
install -m 0555 "${script_dir}/files/rc.d/odin" "${stage_dir}/usr/local/etc/rc.d/odin"

cp "${script_dir}/+PRE_INSTALL" "${meta_dir}/+PRE_INSTALL"
cp "${script_dir}/+POST_INSTALL" "${meta_dir}/+POST_INSTALL"
cp "${script_dir}/+DISPLAY" "${meta_dir}/+DISPLAY"

flatsize=$(find "${stage_dir}" -type f -exec stat -f '%z' {} + | awk '{ total += $1 } END { print total + 0 }')
sed \
	-e "s/^version: .*/version: \"${version}\"/" \
	-e "s/^flatsize: .*/flatsize: ${flatsize}/" \
	"${script_dir}/+MANIFEST" > "${meta_dir}/+MANIFEST"

{
	find "${stage_dir}" \( -type f -o -type l \) -print | sed "s#^${stage_dir}##" | sort
	printf '@dir /opt/odin/etc/odin/services\n'
	printf '@dir /var/db/odin\n'
	printf '@dir /var/log/odin\n'
} > "${plist}"

if [ "${stage_only}" -eq 1 ]; then
	echo "fake root prepared at ${stage_dir}"
	echo "metadata prepared at ${meta_dir}"
	echo "packing list prepared at ${plist}"
	exit 0
fi

pkg create -r "${stage_dir}" -m "${meta_dir}" -p "${plist}" -o "${out_dir}"

echo "created ${out_dir}/${package_name}.pkg"
echo "fake root kept at ${stage_dir}"
