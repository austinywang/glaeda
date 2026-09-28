#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

if [[ $EUID -ne 0 ]]; then
  printf 'error: disposable package probe must run as uid 0\n' >&2
  exit 1
fi
if [[ ${GLAEDA_DISPOSABLE_PROBE:-} != 1 ]]; then
  printf 'error: disposable package probe requires the explicit dangerous integration gate\n' >&2
  exit 1
fi

for command in \
  /usr/bin/dpkg-query \
  /usr/bin/findmnt \
  /usr/bin/git \
  /usr/bin/jq \
  /usr/bin/mount \
  /usr/bin/podman \
  /usr/bin/python3 \
  /usr/bin/readlink \
  /usr/sbin/runuser \
  /usr/bin/stat \
  /usr/bin/systemctl \
  /usr/bin/systemd-run \
  /usr/bin/umount; do
  if [[ ! -x $command ]]; then
    printf 'error: required executable is absent: %s\n' "$command" >&2
    exit 1
  fi
done

case "$(/usr/bin/uname -m)" in
  aarch64 | x86_64) ;;
  *)
    printf 'error: unsupported probe architecture\n' >&2
    exit 1
    ;;
esac

probe_root=$(/usr/bin/mktemp -d /tmp/smolrunner-podman-closure.XXXXXX)
probe_nonce=${probe_root##*.}
probe_nonce=${probe_nonce,,}
probe_user="smolprobe_$probe_nonce"
probe_unit="smolrunner-podman-closure-$probe_nonce.service"
target_mount="$probe_root/target-tmpfs"
target_mounted=0
probe_user_created=0
probe_unit_owned=0

cleanup() {
  local status=$?
  set +e
  if [[ $probe_unit_owned -eq 1 ]]; then
    /usr/bin/systemctl stop "$probe_unit" >/dev/null 2>&1
  fi
  if [[ $target_mounted -eq 1 ]] && /usr/bin/findmnt -rn --target "$target_mount" >/dev/null 2>&1; then
    /usr/bin/umount "$target_mount"
  fi
  if [[ $probe_user_created -eq 1 ]]; then
    /usr/sbin/userdel "$probe_user" >/dev/null 2>&1
  fi
  case "$probe_root" in
    /tmp/smolrunner-podman-closure.*) /usr/bin/rm -rf -- "$probe_root" ;;
  esac
  exit "$status"
}
trap cleanup EXIT

if /usr/bin/id "$probe_user" >/dev/null 2>&1; then
  printf 'error: unique disposable probe user already exists\n' >&2
  exit 1
fi
if ! unit_load_state=$(/usr/bin/systemctl show --property=LoadState --value "$probe_unit" 2>/dev/null) ||
  [[ $unit_load_state != not-found ]]; then
  printf 'error: unique disposable probe unit is not proven absent\n' >&2
  exit 1
fi

assert_root_file() {
  local path=$1
  local owner group mode kind links
  if [[ -L $path ]]; then
    printf 'error: package executable path is a symlink: %s\n' "$path" >&2
    exit 1
  fi
  read -r owner group mode links kind < <(/usr/bin/stat -Lc '%u %g %a %h %F' "$path")
  if [[ $owner != 0 || $group != 0 || $kind != 'regular file' ]]; then
    printf 'error: package file metadata is unsafe: %s\n' "$path" >&2
    exit 1
  fi
  if (( (8#$mode & 0022) != 0 )); then
    printf 'error: package file is group/world writable: %s\n' "$path" >&2
    exit 1
  fi
  if ((links != 1)); then
    printf 'error: package file is not single-link: %s\n' "$path" >&2
    exit 1
  fi
}

printf 'probe_architecture=%s\n' "$(/usr/bin/uname -m)"
/usr/bin/dpkg-query -W -f='package=${Package} version=${Version}\n' \
  aardvark-dns busybox-static catatonit conmon crun fuse-overlayfs git netavark podman uidmap

for path in \
  /usr/bin/podman \
  /usr/bin/git \
  /usr/bin/conmon \
  /usr/bin/crun \
  /usr/bin/newuidmap \
  /usr/bin/newgidmap \
  /usr/bin/catatonit \
  /usr/bin/fuse-overlayfs \
  /usr/lib/podman/aardvark-dns \
  /usr/lib/podman/netavark; do
  assert_root_file "$path"
done

read -r fallback_owner fallback_group fallback_kind < <(
  /usr/bin/stat -c '%u %g %F' /usr/libexec/podman/catatonit
)
if [[ ! -L /usr/libexec/podman/catatonit ]] ||
  [[ $fallback_owner != 0 ]] ||
  [[ $fallback_group != 0 ]] ||
  [[ $fallback_kind != 'symbolic link' ]] ||
  [[ $(/usr/bin/readlink -f /usr/libexec/podman/catatonit) != /usr/bin/catatonit ]]; then
  printf 'error: packaged Podman catatonit fallback is not the exact expected symlink\n' >&2
  exit 1
fi

for helper in /usr/bin/newuidmap /usr/bin/newgidmap; do
  helper_mode=$(/usr/bin/stat -Lc %a "$helper")
  if (( (8#$helper_mode & 04000) == 0 )); then
    printf 'error: namespace helper is not setuid: %s\n' "$helper" >&2
    exit 1
  fi
done

if [[ -e /etc/containers/podman_preexec_hooks.txt ]] ||
  [[ -L /etc/containers/podman_preexec_hooks.txt ]]; then
  printf 'error: Podman pre-exec hook indicator is present\n' >&2
  exit 1
fi
for hook_directory in /usr/libexec/podman/pre-exec-hooks /etc/containers/pre-exec-hooks; do
  if [[ -e $hook_directory ]] || [[ -L $hook_directory ]]; then
    read -r hook_owner hook_group hook_mode hook_kind < <(
      /usr/bin/stat -c '%u %g %a %F' "$hook_directory"
    )
    if [[ -L $hook_directory ]] ||
      [[ $hook_kind != directory ]] ||
      [[ $hook_owner != 0 ]] ||
      [[ $hook_group != 0 ]] ||
      (( (8#$hook_mode & 0022) != 0 )) ||
      [[ -n $(/usr/bin/find "$hook_directory" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
      printf 'error: Podman pre-exec hook path is not an exact protected empty directory\n' >&2
      exit 1
    fi
  fi
done

podman_version=$(/usr/bin/podman --version)
git_version=$(/usr/bin/git --version)
case "$podman_version" in
  'podman version 4.9.3'*) ;;
  *)
    printf 'error: unexpected Podman baseline: %s\n' "$podman_version" >&2
    exit 1
    ;;
esac
case "$git_version" in
  'git version 2.43.'*) ;;
  *)
    printf 'error: unexpected Git baseline: %s\n' "$git_version" >&2
    exit 1
    ;;
esac

/usr/sbin/useradd --create-home --home-dir "$probe_root/hostile-home" --shell /bin/bash "$probe_user"
probe_user_created=1
probe_uid=$(/usr/bin/id -u "$probe_user")
probe_gid=$(/usr/bin/id -g "$probe_user")

if [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subuid) != 1 ]] ||
  [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subgid) != 1 ]]; then
  printf 'error: probe user lacks one exact subordinate ID authority row\n' >&2
  exit 1
fi

mkdir -p \
  "$probe_root/empty-home" \
  "$probe_root/empty-xdg" \
  "$probe_root/git-source" \
  "$probe_root/git-synthetic/objects" \
  "$probe_root/git-synthetic/refs/heads" \
  "$probe_root/git-synthetic/refs/tags" \
  "$probe_root/git-synthetic-other/objects" \
  "$probe_root/git-synthetic-other/refs/heads" \
  "$probe_root/git-synthetic-other/refs/tags" \
  "$target_mount"
chmod 0755 "$probe_root" "$probe_root/empty-home" "$probe_root/empty-xdg"
chown -R "$probe_uid:$probe_gid" "$probe_root/git-source"

mkdir -p "$probe_root/hostile-home/.config/containers"
printf 'this is deliberately invalid persistent runner configuration\n' \
  > "$probe_root/hostile-home/.config/containers/containers.conf"
chown -R "$probe_uid:$probe_gid" "$probe_root/hostile-home"

/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git init --quiet "$probe_root/git-source"
printf 'smolrunner closure probe\n' > "$probe_root/git-source/input.txt"
chown "$probe_uid:$probe_gid" "$probe_root/git-source/input.txt"
/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" -c user.name=probe -c user.email=probe.invalid \
  add input.txt
/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" -c user.name=probe -c user.email=probe.invalid \
  commit --quiet -m probe
probe_tree=$(/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" LC_ALL=C PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" rev-parse 'HEAD^{tree}')

for synthetic in "$probe_root/git-synthetic" "$probe_root/git-synthetic-other"; do
  printf '[core]\n\trepositoryformatversion = 0\n\tbare = true\n' > "$synthetic/config"
  printf 'ref: refs/heads/main\n' > "$synthetic/HEAD"
  chmod -R a-w "$synthetic"
done

/usr/bin/python3 - \
  "$probe_uid" \
  "$probe_gid" \
  "$probe_root/git-source/.git/objects" \
  "$probe_root/git-synthetic" \
  "$probe_root/git-synthetic-other" \
  "$probe_tree" <<'PY'
import hashlib
import os
import subprocess
import sys

uid = int(sys.argv[1])
gid = int(sys.argv[2])
objects = sys.argv[3]
synthetic = sys.argv[4]
other = sys.argv[5]
tree = sys.argv[6]

fd = os.open(objects, os.O_RDONLY | os.O_DIRECTORY)

def demote():
    os.setgroups([])
    os.setgid(gid)
    os.setuid(uid)

environment = {
    "GIT_ALLOW_PROTOCOL": "",
    "GIT_ATTR_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_NO_LAZY_FETCH": "1",
    "GIT_NO_REPLACE_OBJECTS": "1",
    "GIT_OBJECT_DIRECTORY": f"/proc/self/fd/{fd}",
    "GIT_OPTIONAL_LOCKS": "0",
    "GIT_TERMINAL_PROMPT": "0",
    "LC_ALL": "C",
}

def run(arguments):
    return subprocess.run(
        arguments,
        input=(tree + "\n").encode("ascii"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        pass_fds=(fd,),
        preexec_fn=demote,
        timeout=5,
        check=False,
    )

explicit = run(["/usr/bin/git", f"--git-dir={synthetic}", "cat-file", "--batch"])
if explicit.returncode != 0:
    raise SystemExit("explicit root-owned synthetic Git directory was not admitted")
header, payload = explicit.stdout.split(b"\n", 1)
object_name, object_type, object_size = header.decode("ascii").split(" ")
size = int(object_size)
content = payload[:size]
if object_name != tree or object_type != "tree" or payload[size:] != b"\n":
    raise SystemExit("cat-file batch returned a noncanonical tree record")
digest = hashlib.sha1(b"tree " + str(size).encode("ascii") + b"\0" + content).hexdigest()
if digest != tree:
    raise SystemExit("materializer tree object digest mismatch")

other_explicit = run(["/usr/bin/git", f"--git-dir={other}", "cat-file", "--batch"])
if other_explicit.returncode != 0:
    raise SystemExit("Git unexpectedly enforced owner safety for another explicit bare directory")

print("git_materializer=closed explicit_owner_gate=smolrunner")
PY

target_mounted=1
/usr/bin/mount -t tmpfs \
  -o "size=1048576,nr_inodes=64,uid=$probe_uid,gid=$probe_gid,mode=0700,nosuid,nodev" \
  smolrunner-probe-target "$target_mount"
target_type=$(/usr/bin/findmnt -rn -o FSTYPE --target "$target_mount")
target_options=$(/usr/bin/findmnt -rn -o OPTIONS --target "$target_mount")
target_blocks=$(/usr/bin/stat -f -c %b "$target_mount")
target_block_size=$(/usr/bin/stat -f -c %S "$target_mount")
target_inodes=$(/usr/bin/stat -f -c %c "$target_mount")
if [[ $target_type != tmpfs ]] ||
  [[ $target_options != *nosuid* ]] ||
  [[ $target_options != *nodev* ]] ||
  (( target_blocks * target_block_size > 1048576 )) ||
  (( target_inodes > 64 )); then
  printf 'error: target tmpfs did not retain its hard bounds\n' >&2
  exit 1
fi
printf 'target_tmpfs=bounded blocks=%s block_size=%s inodes=%s\n' \
  "$target_blocks" "$target_block_size" "$target_inodes"
/usr/bin/umount "$target_mount"
target_mounted=0

has_option() {
  local help_text=$1
  local option=$2
  /usr/bin/grep -Eq -- "(^|[[:space:],])${option}([=[:space:]]|$)" <<< "$help_text"
}

global_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman --help)
create_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman create --help)
start_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman start --help)
for option in --cgroup-manager --conmon --events-backend --hooks-dir --network-config-dir --remote --runtime --tmpdir --transient-store; do
  if ! has_option "$global_help" "$option"; then
    printf 'error: Podman global option is absent: %s\n' "$option" >&2
    exit 1
  fi
done
for option in \
  --cap-drop \
  --cgroup-parent \
  --cgroupns \
  --cidfile \
  --cpus \
  --entrypoint \
  --env-host \
  --hostname \
  --http-proxy \
  --image-volume \
  --init \
  --init-path \
  --ipc \
  --log-driver \
  --memory \
  --memory-swap \
  --mount \
  --name \
  --network \
  --no-healthcheck \
  --no-hosts \
  --pid \
  --pids-limit \
  --privileged \
  --pull \
  --read-only \
  --read-only-tmpfs \
  --restart \
  --security-opt \
  --shm-size \
  --systemd \
  --tmpfs \
  --user \
  --userns \
  --uts \
  --workdir; do
  if ! has_option "$create_help" "$option"; then
    printf 'error: Podman create option is absent: %s\n' "$option" >&2
    exit 1
  fi
done
if ! has_option "$start_help" --attach; then
  printf 'error: Podman start boolean attach option is absent\n' >&2
  exit 1
fi
attach_invalid=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  /usr/bin/podman start --attach=stdout --help 2>&1) && {
  printf 'error: Podman start attach unexpectedly accepts stream names\n' >&2
  exit 1
}
if [[ $attach_invalid != *ParseBool* ]] ||
  ! /usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  /usr/bin/podman start --attach=true --help >/dev/null; then
  printf 'error: Podman start attach is not the expected Boolean option\n' >&2
  exit 1
fi
printf 'podman_cli_surface=expected\n'

mkdir -p \
  "$probe_root/auth" \
  "$probe_root/config" \
  "$probe_root/graphroot" \
  "$probe_root/hooks" \
  "$probe_root/network" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chown -R "$probe_uid:$probe_gid" \
  "$probe_root/graphroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chmod 0700 \
  "$probe_root/graphroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
printf '' > "$probe_root/config/containers.conf"
printf 'unqualified-search-registries = []\nshort-name-mode = "enforcing"\n' \
  > "$probe_root/config/registries.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s"\ngraphroot = "%s"\nrootless_storage_path = "%s"\n\n[storage.options]\nadditionalimagestores = []\n' \
  "$probe_root/runtime/containers" "$probe_root/graphroot" "$probe_root/graphroot" \
  > "$probe_root/config/storage.conf"
printf '{}\n' > "$probe_root/auth/auth.json"
printf '' > "$probe_root/network/cni.lock"
printf '' > "$probe_root/network/netavark.lock"
chown "$probe_uid:$probe_gid" \
  "$probe_root/network/cni.lock" \
  "$probe_root/network/netavark.lock"
chmod 0600 "$probe_root/network/cni.lock" "$probe_root/network/netavark.lock"
chmod 0555 "$probe_root/network"
chmod -R a-w "$probe_root/auth" "$probe_root/config" "$probe_root/hooks"

cat > "$probe_root/user-probe.sh" <<'USER_PROBE'
#!/usr/bin/env bash
set -euo pipefail

validate_network_lock() {
  local path=$1
  local owner mode links size
  read -r owner mode links size < <(/usr/bin/stat -Lc '%u %a %h %s' "$path")
  if [[ -L $path ]] ||
    [[ ! -f $path ]] ||
    [[ $owner != "$PROBE_UID" ]] ||
    [[ $mode != 600 ]] ||
    [[ $links != 1 ]] ||
    (( size != 0 )); then
    return 1
  fi
}

mapfile -d '' network_entries_before < <(
  /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
)
if [[ ${#network_entries_before[@]} -ne 2 ]] ||
  ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
  ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
  printf 'error: exact precreated network lock state was absent before first use\n' >&2
  exit 1
fi
read -r network_owner network_group network_mode network_kind < <(
  /usr/bin/stat -Lc '%u %g %a %F' "$PROBE_NETWORK"
)
if [[ -L $PROBE_NETWORK ]] ||
  [[ $network_owner != 0 ]] ||
  [[ $network_group != 0 ]] ||
  [[ $network_mode != 555 ]] ||
  [[ $network_kind != directory ]]; then
  printf 'error: network lock directory was not protected before first use\n' >&2
  exit 1
fi
network_identity=$(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK")
cni_lock_identity=$(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK/cni.lock")
netavark_lock_identity=$(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK/netavark.lock")

/usr/bin/podman \
  --remote=false \
  --runtime=/usr/bin/crun \
  --conmon=/usr/bin/conmon \
  --events-backend=none \
  --hooks-dir="$PROBE_HOOKS" \
  --network-config-dir="$PROBE_NETWORK" \
  --cgroup-manager=cgroupfs \
  --tmpdir="$TMPDIR" \
  --transient-store \
  info --format json > "$PROBE_INFO"

if ! /usr/bin/jq -e \
  --arg graphroot "$PROBE_GRAPHROOT" \
  --arg runroot "$PROBE_RUNROOT" \
  '.host.security.rootless == true and .store.graphRoot == $graphroot and .store.runRoot == $runroot' \
  "$PROBE_INFO" >/dev/null; then
  /usr/bin/jq -r \
    --arg graphroot "$PROBE_GRAPHROOT" \
    --arg runroot "$PROBE_RUNROOT" \
    '"rootless=\(.host.security.rootless == true) graphroot_match=\(.store.graphRoot == $graphroot) runroot_match=\(.store.runRoot == $runroot)"' \
    "$PROBE_INFO" >&2
  printf 'error: Podman did not report the exact rootless run-private storage roots\n' >&2
  exit 1
fi

if [[ $(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK") != "$network_identity" ]] ||
  [[ -L $PROBE_NETWORK ]]; then
  printf 'error: run-private network directory identity changed during first use\n' >&2
  exit 1
fi
mapfile -d '' network_entries < <(
  /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
)
if [[ ${#network_entries[@]} -ne 2 ]] ||
  [[ $(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK/cni.lock") != "$cni_lock_identity" ]] ||
  [[ $(/usr/bin/stat -Lc '%d:%i' "$PROBE_NETWORK/netavark.lock") != "$netavark_lock_identity" ]] ||
  ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
  ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
  printf 'network_entry_count=%s\n' "${#network_entries[@]}" >&2
  if (( ${#network_entries[@]} <= 8 )); then
    for network_entry in "${network_entries[@]}"; do
      network_name=${network_entry##*/}
      if (( ${#network_name} <= 64 )); then
        printf 'network_entry_name=%q kind=%s\n' \
          "$network_name" "$(/usr/bin/stat -Lc %F "$network_entry")" >&2
      fi
    done
  fi
  printf 'error: Podman created unexpected run-private network state\n' >&2
  exit 1
fi
printf 'network_state=precreated-and-stable entries=2\n'

pause_file="$XDG_RUNTIME_DIR/libpod/tmp/pause.pid"
if [[ -L $pause_file ]] || [[ ! -f $pause_file ]]; then
  printf 'error: first use did not create the exact rootless pause PID file\n' >&2
  exit 1
fi
read -r pause_owner pause_group pause_mode pause_links pause_size pause_identity pause_kind < <(
  /usr/bin/stat -Lc '%u %g %a %h %s %d:%i %F' "$pause_file"
)
pause_pid=$(/usr/bin/tr -d '\n' < "$pause_file")
if [[ $pause_owner != "$PROBE_UID" ]] || [[ $pause_group != "$PROBE_GID" ]] ||
  [[ $pause_mode != 600 ]] || [[ $pause_links != 1 ]] ||
  (( pause_size == 0 || pause_size > 20 )) || [[ $pause_kind != 'regular file' ]] ||
  [[ ! $pause_pid =~ ^[1-9][0-9]*$ ]] || [[ ! -r /proc/$pause_pid/status ]]; then
  printf 'error: pause PID file identity or process evidence is invalid\n' >&2
  exit 1
fi
pause_uid=$(/usr/bin/awk '/^Uid:/ { print $2 }' "/proc/$pause_pid/status")
mapfile -t pause_cgroups < <(
  /usr/bin/awk -F: '$1 == "0" && $2 == "" { print $3 }' "/proc/$pause_pid/cgroup"
)
mapfile -t service_cgroups < <(
  /usr/bin/awk -F: '$1 == "0" && $2 == "" { print $3 }' /proc/self/cgroup
)
if [[ $pause_uid != "$PROBE_UID" ]] || [[ ${#pause_cgroups[@]} -ne 1 ]] ||
  [[ ${#service_cgroups[@]} -ne 1 ]] || [[ ${pause_cgroups[0]} != "${service_cgroups[0]}" ]] ||
  [[ ${service_cgroups[0]} != "/system.slice/$PROBE_UNIT" ]]; then
  printf 'error: rootless pause process escaped the disposable service cgroup\n' >&2
  exit 1
fi
printf '%s %s\n' "$pause_pid" "$pause_identity" > "$PROBE_PAUSE_RECORD"
printf '%s\n' "${service_cgroups[0]}" > "$PROBE_SERVICE_CGROUP_RECORD"
printf 'rootless_info=success pause=contained crash=armed\n'
kill -KILL "$BASHPID"
printf 'error: disposable crash injection unexpectedly returned\n' >&2
exit 1
USER_PROBE
chmod 0555 "$probe_root/user-probe.sh"

probe_unit_owned=1
set +e
/usr/bin/systemd-run \
  --wait \
  --collect \
  --pipe \
  --service-type=exec \
  --unit="${probe_unit%.service}" \
  --uid="$probe_user" \
  --property=Delegate=yes \
  --property=KillMode=control-group \
  /usr/bin/env -i \
  CONTAINERS_CONF="$probe_root/config/containers.conf" \
  CONTAINERS_REGISTRIES_CONF="$probe_root/config/registries.conf" \
  CONTAINERS_STORAGE_CONF="$probe_root/config/storage.conf" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=$probe_root/runtime/absent-user-bus" \
  HOME="$probe_root/empty-home" \
  LC_ALL=C \
  LOGNAME="$probe_user" \
  PATH=/usr/bin \
  PROBE_HOOKS="$probe_root/hooks" \
  PROBE_GRAPHROOT="$probe_root/graphroot" \
  PROBE_INFO="$probe_root/runtime/info.json" \
  PROBE_NETWORK="$probe_root/network" \
  PROBE_PAUSE_RECORD="$probe_root/runtime/pause.record" \
  PROBE_RUNROOT="$probe_root/runtime/containers" \
  PROBE_SERVICE_CGROUP_RECORD="$probe_root/runtime/service.cgroup" \
  PROBE_GID="$probe_gid" \
  PROBE_UID="$probe_uid" \
  PROBE_UNIT="$probe_unit" \
  REGISTRY_AUTH_FILE="$probe_root/auth/auth.json" \
  TMPDIR="$probe_root/tmp" \
  USER="$probe_user" \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  XDG_RUNTIME_DIR="$probe_root/runtime" \
  /usr/bin/bash "$probe_root/user-probe.sh"
probe_status=$?
set -e

if (( probe_status != 255 )); then
  printf 'probe_status=%s\n' "$probe_status" >&2
  printf 'error: disposable pause-process crash injection did not report exact SIGKILL status\n' >&2
  exit 1
fi

service_cgroup=$(< "$probe_root/runtime/service.cgroup")
if [[ $service_cgroup != "/system.slice/$probe_unit" ]] ||
  [[ -e "/sys/fs/cgroup$service_cgroup" ]] || [[ -L "/sys/fs/cgroup$service_cgroup" ]]; then
  printf 'error: crashed disposable service cgroup was not collected exactly\n' >&2
  exit 1
fi
if ! unit_load_state=$(/usr/bin/systemctl show --property=LoadState --value "$probe_unit" 2>/dev/null) ||
  [[ $unit_load_state != not-found ]]; then
  printf 'error: crashed disposable service unit remained after collection\n' >&2
  exit 1
fi
probe_unit_owned=0

if /usr/bin/pgrep -u "$probe_uid" >/dev/null 2>&1; then
  printf 'error: crashed rootless first-use probe left an idle process\n' >&2
  exit 1
fi

pause_parent="$probe_root/runtime/libpod/tmp"
pause_file="$pause_parent/pause.pid"
for pause_directory in \
  "$probe_root/runtime" \
  "$probe_root/runtime/libpod" \
  "$pause_parent"; do
  if [[ -L $pause_directory ]] || [[ ! -d $pause_directory ]]; then
    printf 'error: pause recovery directory is absent or rebound\n' >&2
    exit 1
  fi
  read -r directory_owner directory_group directory_mode < <(
    /usr/bin/stat -Lc '%u %g %a' "$pause_directory"
  )
  if [[ $directory_owner != "$probe_uid" ]] || [[ $directory_group != "$probe_gid" ]] ||
    (( (8#$directory_mode & 0022) != 0 )); then
    printf 'error: pause recovery directory metadata is unsafe\n' >&2
    exit 1
  fi
done
read -r recorded_pause_pid recorded_pause_identity record_extra < "$probe_root/runtime/pause.record"
if [[ -n ${record_extra:-} ]] || [[ ! $recorded_pause_pid =~ ^[1-9][0-9]*$ ]] ||
  [[ ! $recorded_pause_identity =~ ^[0-9]+:[0-9]+$ ]] ||
  [[ -L $pause_file ]] || [[ ! -f $pause_file ]]; then
  printf 'error: stale pause recovery record or fixed PID file is unsafe\n' >&2
  exit 1
fi
read -r pause_owner pause_group pause_mode pause_links pause_size pause_identity pause_kind < <(
  /usr/bin/stat -Lc '%u %g %a %h %s %d:%i %F' "$pause_file"
)
pause_pid=$(< "$pause_file")
if [[ $pause_owner != "$probe_uid" ]] || [[ $pause_group != "$probe_gid" ]] ||
  [[ $pause_mode != 600 ]] || [[ $pause_links != 1 ]] ||
  (( pause_size == 0 || pause_size > 20 )) || [[ $pause_kind != 'regular file' ]] ||
  [[ $pause_identity != "$recorded_pause_identity" ]] || [[ $pause_pid != "$recorded_pause_pid" ]]; then
  printf 'error: stale pause PID file no longer matches the pre-crash exact inode\n' >&2
  exit 1
fi
/usr/bin/rm -- "$pause_file"
/usr/bin/env -i PATH=/usr/bin /usr/bin/python3 -I -S -c '
import os
import sys

fd = os.open(sys.argv[1], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    os.fsync(fd)
finally:
    os.close(fd)
' "$pause_parent"
if [[ -e $pause_file ]] || [[ -L $pause_file ]] ||
  /usr/bin/find "$probe_root/runtime" -type f -name pause.pid -print -quit | /usr/bin/grep -q .; then
  printf 'error: stale pause PID file remained after exact recovery cleanup\n' >&2
  exit 1
fi
if /usr/bin/findmnt -rn -o TARGET | /usr/bin/grep -Fq "$probe_root"; then
  printf 'error: rootless first-use probe left a mount below its run-private root\n' >&2
  exit 1
fi

printf 'pause_crash_recovery=stale-pid-removed-and-synced\n'
printf 'podman_closure_package_probe=pass\n'
