#!/bin/bash
# Build the marine-lume-container HaLOS app from a published Lume release.
#
#   scripts/build-lume-container.sh v0.12.1 [--output dir] [--pi <ssh-host> [--install-only]]
#
# 1. Downloads lume-<tag>-aarch64-unknown-linux-gnu.tar.gz from the GitHub
#    release and checks it against the release's SHA256SUMS.
# 2. Builds deepbluedynamics/lume:<version> for linux/arm64 (COPY-only
#    Dockerfile, no emulation) and saves it as lume-image-<version>-arm64.tar.gz.
# 3. Generates marine-lume-container_<version>-1_arm64.deb from
#    deploy/halos/marine-lume-container with HaLOS's generate-container-packages,
#    run in a debian:trixie container from apt.halos.fi.
# 4. With --pi, copies both to that host, loads the image, installs the package
#    with apt and checks that lume answers. --install-only skips steps 1-3 and
#    installs what an earlier run left in the output directory.
#
# Needs docker (with buildx), curl, tar and sha256sum. Nothing is compiled.
set -euo pipefail

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; }

repo_slug="DeepBlueDynamics/lume"
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
tag=""
out="$root/dist/halos"
pi=""
install_only=false
while [ $# -gt 0 ]; do
    case "$1" in
        -o|--output) out="$2"; shift 2 ;;
        --pi) pi="$2"; shift 2 ;;
        --install-only) install_only=true; shift ;;
        -h|--help) usage; exit 0 ;;
        v[0-9]*) tag="$1"; shift ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done
[ -n "$tag" ] || { usage >&2; exit 2; }
version="${tag#v}"
image="deepbluedynamics/lume:$version"
image_tar="lume-image-$version-arm64.tar.gz"
deb="marine-lume-container_${version}-1_arm64.deb"

# Docker Desktop on Windows needs Windows paths for bind mounts and build contexts.
hostpath() { if command -v cygpath > /dev/null 2>&1; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }
docker() { MSYS_NO_PATHCONV=1 command docker "$@"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
out="$(cd "$out" && pwd)"

if [ "$install_only" = false ]; then
echo "== 1/4 download $tag arm64 binary and verify it"
asset="lume-$tag-aarch64-unknown-linux-gnu.tar.gz"
base="https://github.com/$repo_slug/releases/download/$tag"
curl -fsSL --retry 3 -o "$work/$asset" "$base/$asset"
curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/SHA256SUMS"
(cd "$work" && grep " \*\{0,1\}$asset\$" SHA256SUMS | sha256sum -c -)
tar -xzf "$work/$asset" -C "$work"
ctx="$work/image"
cp -r "$root/deploy/halos/marine-lume-container/image" "$ctx"
cp "$work/lume-$tag-aarch64-unknown-linux-gnu/lume" "$ctx/lume"

echo "== 2/4 build $image (linux/arm64)"
docker buildx build --platform linux/arm64 -t "$image" \
    --output "type=docker,dest=$(hostpath "$work/image.tar")" "$(hostpath "$ctx")"
gzip -c "$work/image.tar" > "$out/$image_tar"
echo "image: $out/$image_tar ($(du -h "$out/$image_tar" | cut -f1))"

echo "== 3/4 generate $deb"
app="$work/app/marine-lume-container"
mkdir -p "$app"
cp -r "$root/deploy/halos/marine-lume-container/." "$app/"
rm -rf "$app/image"
sed -i -E "s/^version: .*/version: ${version}-1/; s/^upstream_version: .*/upstream_version: ${version}/; s#deepbluedynamics/lume:[0-9][^ \"]*#${image}#g; s/marine-lume \(>= [^)]*\)/marine-lume (>= ${version})/" "$app/metadata.yaml"
sed -i -E "s#deepbluedynamics/lume:[0-9][^ \"}]*#${image}#g" "$app/docker-compose.yml" "$app/config.yml"
# HaLOS packaging tools, installed once into a local arm64 image (about 3 min under
# emulation) and reused; delete the image to pick up a newer tools release.
packager=lume-halos-packager:trixie
if ! docker image inspect "$packager" > /dev/null 2>&1; then
    echo "building $packager (one time)"
    docker build --platform linux/arm64 -t "$packager" - << 'EOF'
FROM debian:trixie
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && apt-get install -y -qq curl gpg ca-certificates > /dev/null \
 && curl -fsSL https://apt.halos.fi/halos-apt-key.asc | gpg --dearmor -o /usr/share/keyrings/halos.gpg \
 && echo "deb [signed-by=/usr/share/keyrings/halos.gpg] https://apt.halos.fi trixie-stable main" > /etc/apt/sources.list.d/halos.list \
 && apt-get update -qq && apt-get install -y -qq container-packaging-tools debhelper > /dev/null \
 && rm -rf /var/lib/apt/lists/*
EOF
fi
docker run --rm -v "$(hostpath "$work/app"):/app" -v "$(hostpath "$out"):/out" --platform linux/arm64 "$packager" bash -c '
    set -euo pipefail
    generate-container-packages --prefix marine -o /out /app/marine-lume-container
    chown "$(stat -c %u:%g /out)" /out/*.deb'
rm -f "$out"/marine-lume-container_*.buildinfo "$out"/marine-lume-container_*.changes
[ -f "$out/$deb" ] || { echo "expected $out/$deb was not generated" >&2; ls -l "$out" >&2; exit 1; }
echo "package: $out/$deb ($(du -h "$out/$deb" | cut -f1))"
fi

[ -n "$pi" ] || { echo "== 4/4 skipped (no --pi)"; exit 0; }

echo "== 4/4 install on $pi"
ssh_opts=()
[ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ] && ssh_opts=(-F "$LUME_DEPLOY_SSH_CONFIG")
scp -q "${ssh_opts[@]}" "$out/$image_tar" "$out/$deb" "$pi:/tmp/"
# shellcheck disable=SC2029 # the file names are expanded locally on purpose
ssh "${ssh_opts[@]}" "$pi" "set -e
    sudo -n docker load -i /tmp/$image_tar
    sudo -n apt-get install -y --reinstall /tmp/$deb 2>&1 | tail -5
    rm -f /tmp/$image_tar /tmp/$deb
    sudo -n systemctl enable marine-lume-container.service
    sudo -n systemctl restart marine-lume-container.service
    for i in \$(seq 1 45); do
        curl -fs -m 3 -o /dev/null http://127.0.0.1:5863/ti/status && break
        sleep 4
    done
    echo \"service: \$(systemctl is-active marine-lume-container.service)\"
    sudo -n docker ps --filter name=^lume\$ --format '{{.Names}} {{.Status}}'
    curl -fsS -m 5 -o /dev/null -w 'query server: HTTP %{http_code}\n' http://127.0.0.1:5863/ti/status"
