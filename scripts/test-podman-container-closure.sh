#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

podman_probe() {
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
    "$@"
}

validate_network_lock() {
  local path=$1
  local owner mode links size
  read -r owner mode links size < <(/usr/bin/stat -Lc '%u %a %h %s' "$path")
  [[ ! -L $path ]] &&
    [[ -f $path ]] &&
    [[ $owner == "$PROBE_UID" ]] &&
    [[ $mode == 600 ]] &&
    [[ $links == 1 ]] &&
    (( size == 0 ))
}

validate_cgroup_limits() {
  local cgroup_dir=$1
  local cpu_max memory_max memory_swap_max pids_max
  cpu_max=$(< "$cgroup_dir/cpu.max")
  memory_max=$(< "$cgroup_dir/memory.max")
  memory_swap_max=$(< "$cgroup_dir/memory.swap.max")
  pids_max=$(< "$cgroup_dir/pids.max")
  [[ $cpu_max == '50000 100000' ]] &&
    [[ $memory_max == 67108864 ]] &&
    [[ $memory_swap_max == 0 ]] &&
    [[ $pids_max == 32 ]]
}

prepare_cgroup_hierarchy() {
  local controller service_cgroup service_cgroup_dir
  local -a available_controllers enabled_controllers unified_cgroups

  mapfile -t unified_cgroups < <(
    /usr/bin/awk -F: '$1 == "0" && $2 == "" { print $3 }' /proc/self/cgroup
  )
  if [[ ${#unified_cgroups[@]} -ne 1 ]]; then
    printf 'error: disposable service lacks one exact unified cgroup identity\n' >&2
    exit 1
  fi
  service_cgroup=${unified_cgroups[0]}
  if [[ ! $service_cgroup =~ ^/[A-Za-z0-9_.@:-]+(/[A-Za-z0-9_.@:-]+)*$ ]] ||
    [[ $service_cgroup == *'/../'* ]] || [[ $service_cgroup == */.. ]] ||
    [[ $service_cgroup == *'/./'* ]] || [[ $service_cgroup == */. ]]; then
    printf 'error: disposable service cgroup identity is noncanonical\n' >&2
    exit 1
  fi
  service_cgroup_dir="/sys/fs/cgroup$service_cgroup"
  if [[ -L $service_cgroup_dir ]] || [[ ! -d $service_cgroup_dir ]]; then
    printf 'error: disposable service cgroup is absent or rebound\n' >&2
    exit 1
  fi
  if [[ $(< "$service_cgroup_dir/cpu.max") != '75000 100000' ]] ||
    [[ $(< "$service_cgroup_dir/memory.max") != 100663296 ]] ||
    [[ $(< "$service_cgroup_dir/memory.swap.max") != 0 ]] ||
    [[ $(< "$service_cgroup_dir/pids.max") != 64 ]]; then
    printf 'error: disposable outer cgroup limits are not exact\n' >&2
    exit 1
  fi

  read -r -a available_controllers < "$service_cgroup_dir/cgroup.controllers"
  for controller in cpu memory pids; do
    if [[ ! " ${available_controllers[*]} " =~ [[:space:]]$controller[[:space:]] ]]; then
      printf 'error: required delegated cgroup controller is absent: %s\n' "$controller" >&2
      exit 1
    fi
  done

  PROBE_SUPERVISOR_CGROUP_DIR="$service_cgroup_dir/supervisor"
  PROBE_PAYLOAD_CGROUP_DIR="$service_cgroup_dir/payload"
  /usr/bin/mkdir "$PROBE_SUPERVISOR_CGROUP_DIR" "$PROBE_PAYLOAD_CGROUP_DIR"
  printf '%s\n' "$BASHPID" > "$PROBE_SUPERVISOR_CGROUP_DIR/cgroup.procs"
  printf '+cpu +memory +pids\n' > "$service_cgroup_dir/cgroup.subtree_control"
  read -r -a enabled_controllers < "$service_cgroup_dir/cgroup.subtree_control"
  for controller in cpu memory pids; do
    if [[ ! " ${enabled_controllers[*]} " =~ [[:space:]]$controller[[:space:]] ]]; then
      printf 'error: required delegated cgroup controller was not enabled: %s\n' "$controller" >&2
      exit 1
    fi
  done

  printf '50000 100000\n' > "$PROBE_PAYLOAD_CGROUP_DIR/cpu.max"
  printf '67108864\n' > "$PROBE_PAYLOAD_CGROUP_DIR/memory.max"
  printf '0\n' > "$PROBE_PAYLOAD_CGROUP_DIR/memory.swap.max"
  printf '32\n' > "$PROBE_PAYLOAD_CGROUP_DIR/pids.max"
  if ! validate_cgroup_limits "$PROBE_PAYLOAD_CGROUP_DIR"; then
    printf 'error: payload cgroup limits did not persist exactly\n' >&2
    exit 1
  fi

  PROBE_CGROUP_PARENT="$service_cgroup/payload"
  printf '%s\n' "$service_cgroup" > "$PROBE_CGROUP_RECORD"
}

PROBE_OWNED_CONTAINER_ID=

validate_target_tmpfs() {
  local block_size blocks free_blocks free_inodes fs_type inode_limit mode owner group
  fs_type=$(/usr/bin/findmnt --noheadings --output FSTYPE --target "$PROBE_TARGET")
  read -r block_size blocks free_blocks inode_limit free_inodes < <(
    /usr/bin/stat -f -c '%S %b %f %c %d' "$PROBE_TARGET"
  )
  read -r owner group mode < <(/usr/bin/stat -Lc '%u %g %a' "$PROBE_TARGET")
  [[ $fs_type == tmpfs ]] &&
    [[ $owner == "$PROBE_UID" ]] &&
    [[ $group == "$PROBE_GID" ]] &&
    [[ $mode == 700 ]] &&
    (( block_size * blocks == 8388608 )) &&
    (( free_blocks <= blocks )) &&
    (( inode_limit == 64 )) &&
    (( free_inodes <= inode_limit )) &&
    /usr/bin/findmnt --noheadings --output OPTIONS --target "$PROBE_TARGET" |
      /usr/bin/awk -F, '
        {
          for (i = 1; i <= NF; i += 1) option[$i] = 1
        }
        END {
          exit !(option["rw"] && option["nosuid"] && option["nodev"] && !option["noexec"])
        }
      '
}

cleanup_user_probe() {
  local status=$?
  trap - EXIT
  set +e
  if [[ $PROBE_OWNED_CONTAINER_ID =~ ^[0-9a-f]{64}$ ]]; then
    podman_probe rm --force "$PROBE_OWNED_CONTAINER_ID" >/dev/null 2>&1
  fi
  exit "$status"
}

run_image_install_probe() {
  local image_id image_hex image_size

  image_id=$(podman_probe import "$PROBE_ROOTFS_TAR" localhost/smolrunner-closure-fixture:local)
  if [[ ! $image_id =~ ^sha256:[0-9a-f]{64}$ ]]; then
    printf 'error: offline image installation returned a noncanonical identity\n' >&2
    exit 1
  fi
  image_hex=${image_id#sha256:}
  podman_probe image inspect "$image_id" > "$PROBE_IMAGE_JSON"
  image_size=$(/usr/bin/stat -Lc %s "$PROBE_IMAGE_JSON")
  if (( image_size == 0 || image_size > 1048576 )) ||
    ! /usr/bin/jq -e --arg image_hex "$image_hex" '
      length == 1 and
      .[0].Id == $image_hex and
      (.[0].RepoTags | index("localhost/smolrunner-closure-fixture:local")) != null
    ' "$PROBE_IMAGE_JSON" >/dev/null; then
    printf 'error: installed offline image identity was absent, oversized, or mismatched\n' >&2
    exit 1
  fi
  printf '%s\n' "$image_id" > "$PROBE_IMAGE_ID_RECORD"
  printf 'offline_image_install=exact\n'
}

run_hostile_probe() {
  local image_id=$1
  local container_id cgroup_leaf cgroup_leaf_name init_output init_status start_output start_status
  local container_size exit_code free_blocks free_inodes kill_fd log_group log_links log_mode
  local log_owner log_size memory_baseline memory_current pids_max_events shmem_baseline shmem_current
  local payload_empty=0 resource_pressure_seen=0 stopped_seen=0
  local -a matching_cgroups matching_specs

  if ! validate_target_tmpfs ||
    [[ -n $(/usr/bin/find "$PROBE_TARGET" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    printf 'error: hostile attempt target did not begin as one empty bounded tmpfs\n' >&2
    exit 1
  fi

  container_id=$(podman_probe create \
    --pull=never \
    --init \
    --init-path=/usr/bin/catatonit \
    --network=none \
    --no-hosts \
    --ipc=private \
    --shm-size=1048576 \
    --pid=private \
    --uts=private \
    --hostname=smolrunner-verification \
    --read-only \
    --read-only-tmpfs=false \
    --image-volume=ignore \
    --cap-drop=all \
    --security-opt=no-new-privileges \
    --security-opt="seccomp=$PROBE_SECCOMP_PROFILE" \
    --cgroup-parent="$PROBE_CGROUP_PARENT" \
    --cgroupns=private \
    --pids-limit=32 \
    --memory=67108864 \
    --memory-swap=67108864 \
    --cpus=0.5 \
    --env-host=false \
    --http-proxy=false \
    --log-driver=k8s-file \
    --log-opt="path=$PROBE_HOSTILE_LOGFILE" \
    --log-opt=max-size=1048576 \
    --privileged=false \
    --systemd=false \
    --restart=no \
    --no-healthcheck \
    --name=smolrunner-hostile-fixture \
    --cidfile="$PROBE_HOSTILE_CIDFILE" \
    --userns=keep-id:uid=1000,gid=1000 \
    --user=1000:1000 \
    --workdir=/target \
    --entrypoint=/bin/hostile \
    --mount="type=bind,src=$PROBE_TARGET,target=/target,rw" \
    --tmpfs=/tmp:rw,noexec,nosuid,nodev,size=1048576,mode=1777 \
    "$image_id")
  if [[ ! $container_id =~ ^[0-9a-f]{64}$ ]] ||
    [[ $(< "$PROBE_HOSTILE_CIDFILE") != "$container_id" ]]; then
    printf 'error: hostile stopped create returned a noncanonical container identity\n' >&2
    exit 1
  fi
  PROBE_OWNED_CONTAINER_ID=$container_id

  set +e
  /usr/bin/timeout --signal=KILL 20s \
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
    container init "$container_id" </dev/null \
    > "$PROBE_HOSTILE_INIT_STDOUT" 2> "$PROBE_HOSTILE_INIT_STDERR"
  init_status=$?
  set -e
  init_output=$(< "$PROBE_HOSTILE_INIT_STDOUT")
  if (( init_status != 0 )) || [[ $init_output != "$container_id" ]] ||
    [[ -s $PROBE_HOSTILE_INIT_STDERR ]]; then
    printf 'init_status=%s init_output=%q\n' "$init_status" "$init_output" >&2
    printf 'error: hostile bounded init did not return the exact container identity\n' >&2
    exit 1
  fi

  mapfile -d '' matching_cgroups < <(
    /usr/bin/find "$PROBE_PAYLOAD_CGROUP_DIR" -mindepth 1 -maxdepth 1 -type d -print0
  )
  if [[ ${#matching_cgroups[@]} -ne 1 ]]; then
    printf 'error: hostile initialized container lacks one exact payload cgroup leaf\n' >&2
    exit 1
  fi
  cgroup_leaf=${matching_cgroups[0]}
  cgroup_leaf_name=${cgroup_leaf##*/}
  if [[ -L $cgroup_leaf ]] || [[ $cgroup_leaf_name != *"$container_id"* ]] ||
    ! validate_cgroup_limits "$cgroup_leaf"; then
    printf 'error: hostile payload cgroup identity or limits drifted\n' >&2
    exit 1
  fi

  mapfile -d '' matching_specs < <(
    /usr/bin/find "$PROBE_ROOT" -xdev -type f -name config.json \
      -path "*$container_id*" -print0 2>/dev/null
  )
  if [[ ${#matching_specs[@]} -ne 1 ]] ||
    ! /usr/bin/jq -e \
      --arg cgroup_leaf "$cgroup_leaf_name" \
      --arg cgroup_parent "$PROBE_CGROUP_PARENT" \
      --arg target "$PROBE_TARGET" '
      (.linux.cgroupsPath == ($cgroup_parent + "/" + $cgroup_leaf) or
        .linux.cgroupsPath == (($cgroup_parent | ltrimstr("/")) + "/" + $cgroup_leaf)) and
      ([.mounts[] | select(.destination == "/target" and .source == $target and
        (.options | index("rw")) != null and (.options | index("rbind")) != null)] | length) == 1
    ' "${matching_specs[0]:-/dev/null}" >/dev/null; then
    printf 'error: hostile OCI spec did not bind the exact target and cgroup leaf\n' >&2
    exit 1
  fi

  memory_baseline=$(< "$cgroup_leaf/memory.current")
  shmem_baseline=$(/usr/bin/awk '$1 == "shmem" { print $2 }' "$cgroup_leaf/memory.stat")
  pids_max_events=$(/usr/bin/awk '$1 == "max" { print $2 }' "$cgroup_leaf/pids.events")
  if [[ ! $memory_baseline =~ ^[0-9]+$ ]] || [[ ! $shmem_baseline =~ ^[0-9]+$ ]] ||
    [[ $pids_max_events != 0 ]]; then
    printf 'error: hostile payload cgroup did not begin with canonical fresh resource evidence\n' >&2
    exit 1
  fi

  exec {kill_fd}> "$cgroup_leaf/cgroup.kill"

  set +e
  start_output=$(/usr/bin/timeout --signal=KILL 20s \
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
    start "$container_id" </dev/null)
  start_status=$?
  set -e
  if (( start_status != 0 )) || [[ $start_output != "$container_id" ]]; then
    printf 'start_status=%s start_output=%q\n' "$start_status" "$start_output" >&2
    printf 'error: hostile detached start did not return the exact container identity\n' >&2
    exit 1
  fi

  for _ in {1..800}; do
    log_size=0
    [[ -f $PROBE_HOSTILE_LOGFILE ]] && log_size=$(/usr/bin/stat -Lc %s "$PROBE_HOSTILE_LOGFILE")
    pids_max_events=$(/usr/bin/awk '$1 == "max" { print $2 }' "$cgroup_leaf/pids.events")
    memory_current=$(< "$cgroup_leaf/memory.current")
    shmem_current=$(/usr/bin/awk '$1 == "shmem" { print $2 }' "$cgroup_leaf/memory.stat")
    read -r free_blocks free_inodes < <(/usr/bin/stat -f -c '%f %d' "$PROBE_TARGET")
    if (( log_size >= 65536 && log_size <= 1048576 && pids_max_events > 0 &&
      memory_current >= memory_baseline + 7340032 && shmem_current >= shmem_baseline + 7340032 &&
      free_blocks == 0 && free_inodes == 0 )); then
      resource_pressure_seen=1
      break
    fi
    /usr/bin/sleep 0.025
  done
  if (( resource_pressure_seen != 1 )); then
    printf 'hostile_log_bytes=%s pids_max_events=%s memory_baseline=%s memory_current=%s shmem_baseline=%s shmem_current=%s free_blocks=%s free_inodes=%s\n' \
      "$log_size" "$pids_max_events" "$memory_baseline" "$memory_current" "$shmem_baseline" \
      "$shmem_current" "$free_blocks" "$free_inodes" >&2
    printf 'error: hostile payload did not reach every bounded pressure signal\n' >&2
    exit 1
  fi

  printf '1\n' >&"$kill_fd"
  exec {kill_fd}>&-
  for _ in {1..200}; do
    if /usr/bin/grep -Fxq 'populated 0' "$cgroup_leaf/cgroup.events"; then
      payload_empty=1
      break
    fi
    /usr/bin/sleep 0.025
  done
  if (( payload_empty != 1 )); then
    printf 'error: authoritative hostile cgroup kill did not empty the exact leaf\n' >&2
    exit 1
  fi

  for _ in {1..200}; do
    podman_probe container inspect "$container_id" > "$PROBE_HOSTILE_CONTAINER_JSON"
    container_size=$(/usr/bin/stat -Lc %s "$PROBE_HOSTILE_CONTAINER_JSON")
    if (( container_size == 0 || container_size > 1048576 )); then
      printf 'error: hostile completion inspection was absent or oversized\n' >&2
      exit 1
    fi
    if /usr/bin/jq -e 'length == 1 and .[0].State.Status == "stopped" and .[0].State.Running == false' \
      "$PROBE_HOSTILE_CONTAINER_JSON" >/dev/null; then
      stopped_seen=1
      exit_code=$(/usr/bin/jq -r '.[0].State.ExitCode' "$PROBE_HOSTILE_CONTAINER_JSON")
      break
    fi
    /usr/bin/sleep 0.025
  done
  if (( stopped_seen != 1 )) || [[ ${exit_code:-0} == 0 ]]; then
    printf 'stopped_seen=%s exit_code=%s\n' "$stopped_seen" "${exit_code:-unknown}" >&2
    printf 'error: hostile cgroup abort did not produce one failed stopped container\n' >&2
    exit 1
  fi

  podman_probe rm "$container_id" >/dev/null
  PROBE_OWNED_CONTAINER_ID=
  if [[ -e $cgroup_leaf ]] || [[ -L $cgroup_leaf ]] ||
    [[ -n $(< "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.procs") ]] ||
    ! /usr/bin/grep -Fxq 'populated 0' "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.events"; then
    printf 'error: hostile payload cgroup debt remained after exact removal\n' >&2
    exit 1
  fi
  read -r log_owner log_group log_mode log_links log_size < <(
    /usr/bin/stat -Lc '%u %g %a %h %s' "$PROBE_HOSTILE_LOGFILE"
  )
  if [[ -L $PROBE_HOSTILE_LOGFILE ]] || [[ ! -f $PROBE_HOSTILE_LOGFILE ]] ||
    [[ $log_owner != "$PROBE_UID" ]] || [[ $log_group != "$PROBE_GID" ]] ||
    (( (8#$log_mode & 0022) != 0 )) || [[ $log_links != 1 ]] ||
    (( log_size < 65536 || log_size >= 1048576 )); then
    printf 'error: hostile payload log did not remain inside its exact bounded identity\n' >&2
    exit 1
  fi
  if ! validate_target_tmpfs; then
    printf 'error: hostile payload changed the bounded target filesystem identity\n' >&2
    exit 1
  fi
  read -r free_blocks free_inodes < <(/usr/bin/stat -f -c '%f %d' "$PROBE_TARGET")
  if (( free_blocks != 0 || free_inodes != 0 )); then
    printf 'error: hostile payload did not prove both target byte and inode ceilings\n' >&2
    exit 1
  fi

  printf 'hostile_abort=cgroup-kill target_tmpfs=byte-and-inode-bounded\n'
}

run_user_probe() {
  local image_id image_hex container_id container_output expected_output
  local init_output init_status start_output start_status inspect_status logs_status completion_seen=0
  local image_size container_size spec_size spec_path cgroup_leaf cgroup_leaf_name
  local log_owner log_group log_mode log_links log_size stderr_size exit_code
  local seccomp_sha_now
  local -a matching_cgroups

  prepare_cgroup_hierarchy

  if [[ -L $PROBE_GRAPHROOT ]] || [[ ! -d $PROBE_GRAPHROOT ]] ||
    [[ -n $(/usr/bin/find "$PROBE_GRAPHROOT" -mindepth 1 -maxdepth 1 -print -quit) ]] ||
    [[ -e $PROBE_RUNROOT ]] || [[ -L $PROBE_RUNROOT ]]; then
    printf 'error: execution graph/run roots were not fresh before image inspection\n' >&2
    exit 1
  fi
  if ! validate_target_tmpfs ||
    [[ -n $(/usr/bin/find "$PROBE_TARGET" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    printf 'error: bounded writable target is not one exact empty executable tmpfs\n' >&2
    exit 1
  fi

  mapfile -d '' network_entries_before < <(
    /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
  )
  if [[ ${#network_entries_before[@]} -ne 2 ]] ||
    ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
    ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
    printf 'error: exact precreated network lock state was absent before image installation\n' >&2
    exit 1
  fi

  seccomp_sha_now=$(/usr/bin/sha256sum "$PROBE_SECCOMP_PROFILE" | /usr/bin/awk '{ print $1 }')
  if [[ $seccomp_sha_now != "$PROBE_SECCOMP_SHA" ]]; then
    printf 'error: exact packaged seccomp profile changed before container creation\n' >&2
    exit 1
  fi

  image_id=$(< "$PROBE_IMAGE_ID_RECORD")
  if [[ ! $image_id =~ ^sha256:[0-9a-f]{64}$ ]]; then
    printf 'error: sealed offline image record is noncanonical\n' >&2
    exit 1
  fi
  image_hex=${image_id#sha256:}

  podman_probe image inspect "$image_id" > "$PROBE_IMAGE_JSON"
  image_size=$(/usr/bin/stat -Lc %s "$PROBE_IMAGE_JSON")
  if (( image_size == 0 || image_size > 1048576 )) ||
    ! /usr/bin/jq -e --arg image_hex "$image_hex" '
      length == 1 and
      .[0].Id == $image_hex and
      (.[0].RepoTags | index("localhost/smolrunner-closure-fixture:local")) != null
    ' "$PROBE_IMAGE_JSON" >/dev/null; then
    printf 'image_inspect_bytes=%s\n' "$image_size" >&2
    /usr/bin/jq -c 'if length == 1 then {id: .[0].Id, tags: .[0].RepoTags} else {count: length} end' \
      "$PROBE_IMAGE_JSON" >&2 || true
    printf 'error: read-only additional-store image inspection was absent, oversized, or mismatched\n' >&2
    exit 1
  fi
  if ! /usr/bin/grep -Fq "$PROBE_IMAGE_STORE/" "$PROBE_IMAGE_JSON"; then
    printf 'error: image inspection did not bind the read-only additional store\n' >&2
    exit 1
  fi

  container_id=$(podman_probe create \
    --pull=never \
    --init \
    --init-path=/usr/bin/catatonit \
    --network=none \
    --no-hosts \
    --ipc=private \
    --shm-size=1048576 \
    --pid=private \
    --uts=private \
    --hostname=smolrunner-verification \
    --read-only \
    --read-only-tmpfs=false \
    --image-volume=ignore \
    --cap-drop=all \
    --security-opt=no-new-privileges \
    --security-opt="seccomp=$PROBE_SECCOMP_PROFILE" \
    --cgroup-parent="$PROBE_CGROUP_PARENT" \
    --cgroupns=private \
    --pids-limit=32 \
    --memory=67108864 \
    --memory-swap=67108864 \
    --cpus=0.5 \
    --env-host=false \
    --http-proxy=false \
    --log-driver=k8s-file \
    --log-opt="path=$PROBE_LOGFILE" \
    --log-opt=max-size=1048576 \
    --privileged=false \
    --systemd=false \
    --restart=no \
    --no-healthcheck \
    --name=smolrunner-closure-fixture \
    --cidfile="$PROBE_CIDFILE" \
    --userns=keep-id:uid=1000,gid=1000 \
    --user=1000:1000 \
    --workdir=/ \
    --entrypoint=/bin/busybox \
    --tmpfs=/tmp:rw,noexec,nosuid,nodev,size=1048576,mode=1777 \
    "$image_id" \
    sha256sum /etc/passwd /etc/group)
  if [[ ! $container_id =~ ^[0-9a-f]{64}$ ]] ||
    [[ $(< "$PROBE_CIDFILE") != "$container_id" ]]; then
    printf 'error: stopped create returned a noncanonical container identity\n' >&2
    exit 1
  fi
  PROBE_OWNED_CONTAINER_ID=$container_id

  set +e
  /usr/bin/timeout --signal=KILL 20s \
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
    container init "$container_id" </dev/null \
    > "$PROBE_INIT_STDOUT" 2> "$PROBE_INIT_STDERR"
  init_status=$?
  set -e
  init_output=$(< "$PROBE_INIT_STDOUT")
  stderr_size=$(/usr/bin/stat -Lc %s "$PROBE_INIT_STDERR")
  if (( init_status != 0 || stderr_size != 0 )) || [[ $init_output != "$container_id" ]]; then
    printf 'init_status=%s init_output=%q init_stderr_bytes=%s\n' \
      "$init_status" "$init_output" "$stderr_size" >&2
    printf 'error: bounded init did not return the exact stopped container identity\n' >&2
    exit 1
  fi

  podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON"
  container_size=$(/usr/bin/stat -Lc %s "$PROBE_CONTAINER_JSON")
  if (( container_size == 0 || container_size > 1048576 )) ||
    ! /usr/bin/jq -e \
      --arg container_id "$container_id" \
      --arg image_hex "$image_hex" \
      --arg cgroup_parent "$PROBE_CGROUP_PARENT" \
      --arg log_path "$PROBE_LOGFILE" '
      length == 1 and
      .[0].Id == $container_id and
      .[0].Image == $image_hex and
      .[0].State.Status == "initialized" and
      .[0].Config.User == "1000:1000" and
      .[0].Config.WorkingDir == "/" and
      .[0].Config.Entrypoint == "/bin/busybox" and
      .[0].HostConfig.NetworkMode == "none" and
      .[0].HostConfig.ReadonlyRootfs == true and
      .[0].HostConfig.Privileged == false and
      .[0].HostConfig.CgroupParent == $cgroup_parent and
      .[0].HostConfig.PidsLimit == 32 and
      .[0].HostConfig.Memory == 67108864 and
      .[0].HostConfig.MemorySwap == 67108864 and
      .[0].HostConfig.NanoCpus == 500000000 and
      .[0].HostConfig.LogConfig.Type == "k8s-file" and
      .[0].HostConfig.LogConfig.Path == $log_path and
      .[0].HostConfig.LogConfig.Size == "1.049MB"
    ' "$PROBE_CONTAINER_JSON" >/dev/null; then
    /usr/bin/jq -c --arg cgroup_parent "$PROBE_CGROUP_PARENT" '.[0] | {
      state: .State.Status,
      user: .Config.User,
      entrypoint: .Config.Entrypoint,
      network: .HostConfig.NetworkMode,
      readonly: .HostConfig.ReadonlyRootfs,
      privileged: .HostConfig.Privileged,
      cgroup_parent_matches: (.HostConfig.CgroupParent == $cgroup_parent),
      pids: .HostConfig.PidsLimit,
      memory: .HostConfig.Memory,
      swap: .HostConfig.MemorySwap,
      nanocpus: .HostConfig.NanoCpus,
      log: .HostConfig.LogConfig
    }' "$PROBE_CONTAINER_JSON" >&2 || true
    printf 'error: stopped container inspection did not match the closed fixture\n' >&2
    exit 1
  fi
  if /usr/bin/grep -Fq "$PROBE_USER" "$PROBE_CONTAINER_JSON"; then
    printf 'error: stopped container inspection contains the host account name\n' >&2
    exit 1
  fi

  mapfile -d '' matching_cgroups < <(
    /usr/bin/find "$PROBE_PAYLOAD_CGROUP_DIR" -mindepth 1 -maxdepth 1 -type d -print0
  )
  if [[ ${#matching_cgroups[@]} -ne 1 ]]; then
    printf 'payload_cgroup_children=%s\n' "${#matching_cgroups[@]}" >&2
    printf 'error: initialized container lacks one exact payload cgroup leaf\n' >&2
    exit 1
  fi
  cgroup_leaf=${matching_cgroups[0]}
  cgroup_leaf_name=${cgroup_leaf##*/}
  if [[ -L $cgroup_leaf ]] || [[ $cgroup_leaf_name != *"$container_id"* ]] ||
    ! validate_cgroup_limits "$cgroup_leaf"; then
    printf 'cgroup_leaf_name=%q\n' "$cgroup_leaf_name" >&2
    printf 'error: initialized container payload cgroup identity or limits drifted\n' >&2
    exit 1
  fi

  mapfile -d '' matching_specs < <(
    /usr/bin/find "$PROBE_ROOT" -xdev -type f -name config.json \
      -path "*$container_id*" -print0 2>/dev/null
  )
  if [[ ${#matching_specs[@]} -ne 1 ]]; then
    printf 'spec_count=%s\n' "${#matching_specs[@]}" >&2
    printf 'error: exact generated OCI specification was not uniquely available before start\n' >&2
    exit 1
  fi
  spec_path=${matching_specs[0]}
  spec_size=$(/usr/bin/stat -Lc %s "$spec_path")
  if (( spec_size == 0 || spec_size > 1048576 )) ||
    ! /usr/bin/jq -e \
      --arg cgroup_leaf "$cgroup_leaf_name" \
      --arg cgroup_parent "$PROBE_CGROUP_PARENT" \
      --arg probe_root "$PROBE_ROOT" '
      def one_pathless_namespace($kind):
        [.linux.namespaces[] | select(.type == $kind)] as $entries |
        ($entries | length) == 1 and ($entries[0].path? == null);
      def forbidden_environment:
        startswith("PROBE_") or
        startswith("CONTAINERS_") or
        startswith("REGISTRY_AUTH_FILE=") or
        startswith("DBUS_SESSION_BUS_ADDRESS=") or
        startswith("XDG_CONFIG_HOME=") or
        startswith("XDG_RUNTIME_DIR=") or
        startswith("HTTP_PROXY=") or
        startswith("HTTPS_PROXY=") or
        startswith("ALL_PROXY=") or
        startswith("NO_PROXY=") or
        startswith("http_proxy=") or
        startswith("https_proxy=") or
        startswith("all_proxy=") or
        startswith("no_proxy=") or
        contains($probe_root);
      .process.user.uid == 1000 and
      .process.user.gid == 1000 and
      .process.user.umask == 18 and
      .process.user.additionalGids == [1000] and
      .process.noNewPrivileges == true and
      (.process.capabilities | type == "object" and length == 0) and
      (.process.apparmorProfile // "") == "" and
      (.linux.cgroupsPath == ($cgroup_parent + "/" + $cgroup_leaf) or
        .linux.cgroupsPath == (($cgroup_parent | ltrimstr("/")) + "/" + $cgroup_leaf)) and
      one_pathless_namespace("ipc") and
      one_pathless_namespace("pid") and
      one_pathless_namespace("uts") and
      one_pathless_namespace("cgroup") and
      ([.process.env[] | select(forbidden_environment)] | length) == 0 and
      ([.mounts[] | select(.destination == "/etc/passwd" or .destination == "/etc/group")] | length) == 0 and
      .linux.seccomp != null
    ' "$spec_path" >/dev/null; then
    /usr/bin/jq -c \
      --arg cgroup_leaf "$cgroup_leaf_name" \
      --arg cgroup_parent "$PROBE_CGROUP_PARENT" '{
      user: .process.user,
      no_new_privileges: .process.noNewPrivileges,
      capabilities: .process.capabilities,
      cgroup_path_matches: (.linux.cgroupsPath == ($cgroup_parent + "/" + $cgroup_leaf) or
        .linux.cgroupsPath == (($cgroup_parent | ltrimstr("/")) + "/" + $cgroup_leaf)),
      namespaces: [.linux.namespaces[] | {type, path_present: has("path")}],
      environment_names: [.process.env[] | split("=")[0]],
      account_mounts: [.mounts[] | select(.destination == "/etc/passwd" or .destination == "/etc/group") | .destination],
      seccomp_present: (.linux.seccomp != null),
      apparmor: (.process.apparmorProfile // "")
    }' "$spec_path" >&2 || true
    printf 'error: generated OCI spec weakened identity, capabilities, seccomp, or account-file ownership\n' >&2
    exit 1
  fi
  if /usr/bin/find "$PROBE_GRAPHROOT" "$PROBE_RUNROOT" -type f \
    \( -path "*/$container_id/userdata/passwd" -o -path "*/$container_id/userdata/group" \) \
    -print -quit 2>/dev/null | /usr/bin/grep -q .; then
    printf 'error: Podman synthesized runtime passwd/group files\n' >&2
    exit 1
  fi

  set +e
  start_output=$(/usr/bin/timeout --signal=KILL 20s \
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
    start "$container_id" </dev/null)
  start_status=$?
  set -e
  if (( start_status != 0 )) || [[ $start_output != "$container_id" ]]; then
    podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON" || true
    printf 'start_status=%s start_output=%q\n' "$start_status" "$start_output" >&2
    /usr/bin/jq -c 'if length == 1 then {
      state: .[0].State.Status,
      running: .[0].State.Running,
      exit_code: .[0].State.ExitCode,
      error: .[0].State.Error,
      oom: .[0].State.OOMKilled
    } else {count: length} end' "$PROBE_CONTAINER_JSON" >&2 || true
    printf 'error: bounded detached start did not return the exact container identity\n' >&2
    exit 1
  fi

  for _ in {1..200}; do
    set +e
    /usr/bin/timeout --signal=KILL 2s \
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
      container inspect "$container_id" </dev/null > "$PROBE_CONTAINER_JSON"
    inspect_status=$?
    set -e
    if (( inspect_status != 0 )); then
      printf 'inspect_status=%s\n' "$inspect_status" >&2
      printf 'error: bounded completion inspection failed\n' >&2
      exit 1
    fi
    container_size=$(/usr/bin/stat -Lc %s "$PROBE_CONTAINER_JSON")
    if (( container_size == 0 || container_size > 1048576 )); then
      printf 'error: bounded completion inspection was absent or oversized\n' >&2
      exit 1
    fi
    if /usr/bin/jq -e 'length == 1 and .[0].State.Status == "stopped" and .[0].State.Running == false' \
      "$PROBE_CONTAINER_JSON" >/dev/null; then
      completion_seen=1
      exit_code=$(/usr/bin/jq -r '.[0].State.ExitCode' "$PROBE_CONTAINER_JSON")
      break
    fi
    if ! /usr/bin/jq -e 'length == 1 and .[0].State.Status == "running" and .[0].State.Running == true' \
      "$PROBE_CONTAINER_JSON" >/dev/null; then
      printf 'error: completion inspection observed an unexpected container state\n' >&2
      exit 1
    fi
    /usr/bin/sleep 0.05
  done
  if (( completion_seen != 1 )) || [[ ${exit_code:-} != 0 ]]; then
    printf 'completion_seen=%s exit_code=%s\n' "$completion_seen" "${exit_code:-unknown}" >&2
    printf 'error: bounded inspection did not report one clean payload exit\n' >&2
    exit 1
  fi

  set +e
  container_output=$(/usr/bin/timeout --signal=KILL 20s \
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
    logs "$container_id" </dev/null 2>"$PROBE_CAPTURE_STDERR")
  logs_status=$?
  set -e
  stderr_size=$(/usr/bin/stat -Lc %s "$PROBE_CAPTURE_STDERR")
  if (( logs_status != 0 || stderr_size != 0 )); then
    printf 'logs_status=%s logs_stderr_bytes=%s\n' "$logs_status" "$stderr_size" >&2
    printf 'error: bounded log retrieval did not report one clean payload exit\n' >&2
    exit 1
  fi

  expected_output="$PROBE_PASSWD_SHA  /etc/passwd
$PROBE_GROUP_SHA  /etc/group"
  if [[ $container_output != "$expected_output" ]]; then
    printf 'error: in-container account files differ from exact image-owned bytes\n' >&2
    exit 1
  fi

  read -r log_owner log_group log_mode log_links log_size < <(
    /usr/bin/stat -Lc '%u %g %a %h %s' "$PROBE_LOGFILE"
  )
  if [[ -L $PROBE_LOGFILE ]] || [[ ! -f $PROBE_LOGFILE ]] ||
    [[ $log_owner != "$PROBE_UID" ]] || [[ $log_group != "$PROBE_GID" ]] ||
    (( (8#$log_mode & 0022) != 0 )) || [[ $log_links != 1 ]] ||
    (( log_size == 0 || log_size >= 1048576 )); then
    printf 'error: payload log was absent, unsafe, or reached its exact overflow boundary\n' >&2
    exit 1
  fi

  podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON"
  if ! /usr/bin/jq -e 'length == 1 and .[0].State.Status == "stopped" and .[0].State.ExitCode == 0' \
    "$PROBE_CONTAINER_JSON" >/dev/null; then
    printf 'error: trusted account gate did not exit successfully\n' >&2
    exit 1
  fi

  podman_probe rm "$container_id" >/dev/null
  if podman_probe container exists "$container_id"; then
    printf 'error: exact fixture container still exists after removal\n' >&2
    exit 1
  fi
  PROBE_OWNED_CONTAINER_ID=
  if [[ -n $(/usr/bin/find "$PROBE_PAYLOAD_CGROUP_DIR" -mindepth 1 -maxdepth 1 -type d -print -quit) ]] ||
    [[ -n $(< "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.procs") ]] ||
    ! /usr/bin/grep -Fxq 'populated 0' "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.events"; then
    payload_child_count=$(/usr/bin/find "$PROBE_PAYLOAD_CGROUP_DIR" -mindepth 1 -maxdepth 1 -type d | /usr/bin/awk 'END { print NR + 0 }')
    payload_process_count=$(/usr/bin/awk 'END { print NR + 0 }' "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.procs")
    payload_populated=$(/usr/bin/awk '$1 == "populated" { print $2 }' "$PROBE_PAYLOAD_CGROUP_DIR/cgroup.events")
    printf 'payload_child_count=%s payload_process_count=%s payload_populated=%s\n' \
      "$payload_child_count" "$payload_process_count" "${payload_populated:-unknown}" >&2
    printf 'error: exact payload cgroup was not empty after container removal\n' >&2
    exit 1
  fi
  run_hostile_probe "$image_id"
  if podman_probe image rm "$image_id" >/dev/null 2>&1; then
    printf 'error: runner removed an image from the read-only additional store\n' >&2
    exit 1
  fi

  mapfile -d '' network_entries_after < <(
    /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
  )
  if [[ ${#network_entries_after[@]} -ne 2 ]] ||
    ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
    ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
    printf 'error: offline container attempt created unexpected network state\n' >&2
    exit 1
  fi

  printf 'offline_image=exact stopped_create=closed account_files=image-owned cgroup=bounded apparmor=rootless-unavailable\n'
}

if [[ ${GLAEDA_DISPOSABLE_PROBE:-} != 1 ]]; then
  printf 'error: disposable container probe requires the explicit dangerous integration gate\n' >&2
  exit 1
fi

if [[ ${1:-} == --image-install-probe ]]; then
  run_image_install_probe
  exit 0
fi

if [[ ${1:-} == --user-probe ]]; then
  trap cleanup_user_probe EXIT
  run_user_probe
  exit 0
fi

if [[ $EUID -ne 0 ]]; then
  printf 'error: disposable container probe must run as uid 0\n' >&2
  exit 1
fi

snapshot_read_only_image_store() {
  local store=$1 expected_identity=$2 identity link mount_target target
  local -a mount_options

  identity=$(/usr/bin/stat -Lc '%d:%i' "$store")
  mount_target=$(/usr/bin/findmnt -rn -o TARGET --target "$store")
  IFS=, read -r -a mount_options <<< "$(/usr/bin/findmnt -rn -o OPTIONS --target "$store")"
  if [[ -L $store ]] || [[ ! -d $store ]] || [[ $identity != "$expected_identity" ]] ||
    [[ $mount_target != "$store" ]] ||
    [[ ! " ${mount_options[*]} " =~ [[:space:]]ro[[:space:]] ]] ||
    [[ ! " ${mount_options[*]} " =~ [[:space:]]nosuid[[:space:]] ]] ||
    [[ ! " ${mount_options[*]} " =~ [[:space:]]nodev[[:space:]] ]] ||
    [[ -n $(/usr/bin/find "$store" -xdev -type f ! -links 1 -print -quit) ]] ||
    [[ -n $(/usr/bin/find "$store" -xdev ! -type d ! -type f ! -type l -print -quit) ]]; then
    printf 'error: installed image store is not one exact root-controlled read-only mount\n' >&2
    exit 1
  fi
  while IFS= read -r -d '' link; do
    target=$(/usr/bin/readlink -f -- "$link")
    if [[ $target != "$store"/* ]]; then
      printf 'error: installed image store contains an escaping or dangling symlink\n' >&2
      exit 1
    fi
  done < <(/usr/bin/find "$store" -xdev -type l -print0)

  /usr/bin/tar \
    --sort=name \
    --format=gnu \
    --numeric-owner \
    --xattrs \
    --xattrs-include='*' \
    -C "$store" -cf - . |
    /usr/bin/sha256sum |
    /usr/bin/awk '{ print $1 }'
}

for command in \
  /usr/bin/busybox \
  /usr/bin/mkdir \
  /usr/bin/findmnt \
  /usr/bin/jq \
  /usr/bin/mount \
  /usr/bin/podman \
  /usr/bin/readlink \
  /usr/bin/sha256sum \
  /usr/bin/stat \
  /usr/bin/systemctl \
  /usr/bin/systemd-run \
  /usr/bin/tar \
  /usr/bin/timeout \
  /usr/bin/umount; do
  if [[ ! -x $command ]]; then
    printf 'error: required executable is absent: %s\n' "$command" >&2
    exit 1
  fi
done

probe_seccomp_source=/usr/share/containers/seccomp.json
probe_seccomp_expected_sha=cc374cf23846ce1f62f4dc807a8e2b8673c783c6f56cb475467621035d281e6c
if [[ -L $probe_seccomp_source ]] || [[ ! -f $probe_seccomp_source ]]; then
  printf 'error: exact packaged seccomp profile is absent or not a regular file\n' >&2
  exit 1
fi
seccomp_source_sha=$(/usr/bin/sha256sum "$probe_seccomp_source" | /usr/bin/awk '{ print $1 }')
read -r seccomp_owner seccomp_group seccomp_links seccomp_size < <(
  /usr/bin/stat -Lc '%u %g %h %s' "$probe_seccomp_source"
)
if [[ $seccomp_owner != 0 ]] || [[ $seccomp_group != 0 ]] || [[ $seccomp_links != 1 ]] ||
  (( seccomp_size == 0 || seccomp_size > 1048576 )) ||
  [[ $seccomp_source_sha != "$probe_seccomp_expected_sha" ]]; then
  printf 'seccomp_owner=%s seccomp_group=%s seccomp_links=%s seccomp_size=%s seccomp_sha256=%s\n' \
    "$seccomp_owner" "$seccomp_group" "$seccomp_links" "$seccomp_size" \
    "$seccomp_source_sha" >&2
  printf 'error: packaged seccomp source does not match the pinned disposable fixture\n' >&2
  exit 1
fi

case "$(/usr/bin/uname -m)" in
  aarch64 | x86_64) ;;
  *)
    printf 'error: unsupported probe architecture\n' >&2
    exit 1
    ;;
esac

probe_root=$(/usr/bin/mktemp -d /tmp/smolrunner-podman-container.XXXXXX)
probe_nonce=${probe_root##*.}
probe_nonce=${probe_nonce,,}
probe_user="smolctr_$probe_nonce"
probe_install_unit="smolrunner-podman-image-$probe_nonce.service"
probe_unit="smolrunner-podman-container-$probe_nonce.service"
probe_user_created=0
probe_install_unit_owned=0
probe_unit_owned=0
probe_target_mounted=0
probe_image_store_mounted=0
probe_script=$(/usr/bin/readlink -f "$0")
probe_user_script="$probe_root/user-probe.sh"
probe_seccomp_profile="$probe_root/seccomp.json"
probe_target="$probe_root/target"
probe_image_backing="$probe_root/image-backing/store"
probe_image_store="$probe_root/image-store"

cleanup() {
  local status=$?
  set +e
  if [[ $probe_install_unit_owned -eq 1 ]]; then
    /usr/bin/systemctl stop "$probe_install_unit" >/dev/null 2>&1
  fi
  if [[ $probe_unit_owned -eq 1 ]]; then
    /usr/bin/systemctl stop "$probe_unit" >/dev/null 2>&1
  fi
  if [[ $probe_target_mounted -eq 1 ]]; then
    /usr/bin/umount -- "$probe_target" >/dev/null 2>&1
  fi
  if [[ $probe_image_store_mounted -eq 1 ]]; then
    /usr/bin/umount -- "$probe_image_store" >/dev/null 2>&1
  fi
  if [[ $probe_user_created -eq 1 ]]; then
    /usr/sbin/userdel "$probe_user" >/dev/null 2>&1
  fi
  case "$probe_root" in
    /tmp/smolrunner-podman-container.*) /usr/bin/rm -rf -- "$probe_root" ;;
  esac
  exit "$status"
}
trap cleanup EXIT

if /usr/bin/id "$probe_user" >/dev/null 2>&1; then
  printf 'error: unique disposable probe user already exists\n' >&2
  exit 1
fi
for candidate_unit in "$probe_install_unit" "$probe_unit"; do
  if ! unit_load_state=$(/usr/bin/systemctl show --property=LoadState --value "$candidate_unit" 2>/dev/null) ||
    [[ $unit_load_state != not-found ]]; then
    printf 'error: unique disposable probe unit is not proven absent\n' >&2
    exit 1
  fi
done
for mounts_conf in /usr/share/containers/mounts.conf /etc/containers/mounts.conf; do
  if [[ -e $mounts_conf ]] || [[ -L $mounts_conf ]]; then
    if [[ -L $mounts_conf ]] || [[ ! -f $mounts_conf ]] ||
      /usr/bin/awk '!/^[[:space:]]*($|#)/ { found = 1 } END { exit !found }' "$mounts_conf"; then
      printf 'error: ambient system mounts configuration is present\n' >&2
      exit 1
    fi
  fi
done

/usr/sbin/useradd --home-dir "$probe_root/empty-home" --no-create-home --shell /bin/bash "$probe_user"
probe_user_created=1
probe_uid=$(/usr/bin/id -u "$probe_user")
probe_gid=$(/usr/bin/id -g "$probe_user")
if [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subuid) != 1 ]] ||
  [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subgid) != 1 ]]; then
  printf 'error: probe user lacks one exact subordinate ID authority row\n' >&2
  exit 1
fi

mkdir -p \
  "$probe_root/auth" \
  "$probe_root/config" \
  "$probe_root/empty-home" \
  "$probe_root/empty-xdg" \
  "$probe_root/graphroot" \
  "$probe_root/hooks" \
  "$probe_root/image-backing" \
  "$probe_image_backing" \
  "$probe_image_store" \
  "$probe_root/image-network" \
  "$probe_root/image-runtime" \
  "$probe_root/image-tmp" \
  "$probe_root/network" \
  "$probe_root/rootfs/bin" \
  "$probe_root/rootfs/etc" \
  "$probe_root/rootfs/tmp" \
  "$probe_root/runtime" \
  "$probe_target" \
  "$probe_root/tmp"
chmod 0755 "$probe_root"
install -o 0 -g 0 -m 0555 "$probe_script" "$probe_user_script"
install -o 0 -g 0 -m 0555 /usr/bin/busybox "$probe_root/rootfs/bin/busybox"
install -o 0 -g 0 -m 0444 "$probe_seccomp_source" "$probe_seccomp_profile"
printf '%s\n' \
  '#!/bin/busybox sh' \
  'set +e' \
  ': > /target/fill' \
  'index=0' \
  'while [ "$index" -lt 256 ]; do' \
  '  /bin/busybox touch "/target/inode-$index" || break' \
  '  index=$((index + 1))' \
  'done' \
  '/bin/busybox dd if=/dev/zero of=/target/fill bs=1048576 count=16 conv=notrunc' \
  'while :; do' \
  '  printf "smolrunner-hostile-output-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\\n"' \
  'done &' \
  'index=0' \
  'while [ "$index" -lt 64 ]; do' \
  '  /bin/busybox sleep 60 &' \
  '  index=$((index + 1))' \
  'done' \
  'wait' \
  > "$probe_root/rootfs/bin/hostile"
chmod 0555 "$probe_root/rootfs/bin/hostile"
seccomp_sha=$(/usr/bin/sha256sum "$probe_seccomp_profile" | /usr/bin/awk '{ print $1 }')
read -r seccomp_owner seccomp_group seccomp_mode seccomp_links seccomp_size < <(
  /usr/bin/stat -Lc '%u %g %a %h %s' "$probe_seccomp_profile"
)
if [[ -L $probe_seccomp_profile ]] || [[ ! -f $probe_seccomp_profile ]] ||
  [[ $seccomp_owner != 0 ]] || [[ $seccomp_group != 0 ]] || [[ $seccomp_mode != 444 ]] ||
  [[ $seccomp_links != 1 ]] || [[ $seccomp_sha != "$probe_seccomp_expected_sha" ]] ||
  (( seccomp_size == 0 || seccomp_size > 1048576 )); then
  printf 'error: attempt-private seccomp snapshot is not exact and immutable\n' >&2
  exit 1
fi
printf 'root:x:0:0:root:/root:/bin/busybox\nsmolgate:x:1000:1000:SmolRunner gate:/:/bin/busybox\n' \
  > "$probe_root/rootfs/etc/passwd"
printf 'root:x:0:\nsmolgate:x:1000:\n' > "$probe_root/rootfs/etc/group"
chmod 0444 "$probe_root/rootfs/etc/passwd" "$probe_root/rootfs/etc/group"
chmod 1777 "$probe_root/rootfs/tmp"

passwd_sha=$(/usr/bin/sha256sum "$probe_root/rootfs/etc/passwd" | /usr/bin/awk '{ print $1 }')
group_sha=$(/usr/bin/sha256sum "$probe_root/rootfs/etc/group" | /usr/bin/awk '{ print $1 }')
/usr/bin/tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  --format=posix -C "$probe_root/rootfs" -cf "$probe_root/rootfs.tar" .
chmod 0444 "$probe_root/rootfs.tar"

printf '' > "$probe_root/config/containers.conf"
printf 'unqualified-search-registries = []\nshort-name-mode = "enforcing"\n' \
  > "$probe_root/config/registries.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s"\ngraphroot = "%s"\nrootless_storage_path = "%s"\n\n[storage.options]\nadditionalimagestores = []\n' \
  "$probe_root/image-runtime/containers" "$probe_image_backing" "$probe_image_backing" \
  > "$probe_root/config/image-storage.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s"\ngraphroot = "%s"\nrootless_storage_path = "%s"\n\n[storage.options]\nadditionalimagestores = ["%s"]\n' \
  "$probe_root/runtime/containers" "$probe_root/graphroot" "$probe_root/graphroot" \
  "$probe_image_store" > "$probe_root/config/storage.conf"
printf '{}\n' > "$probe_root/auth/auth.json"
printf '' > "$probe_root/image-network/cni.lock"
printf '' > "$probe_root/image-network/netavark.lock"
printf '' > "$probe_root/network/cni.lock"
printf '' > "$probe_root/network/netavark.lock"

chown -R "$probe_uid:$probe_gid" \
  "$probe_root/graphroot" \
  "$probe_image_backing" \
  "$probe_root/image-runtime" \
  "$probe_root/image-tmp" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chown "$probe_uid:$probe_gid" \
  "$probe_root/image-network/cni.lock" \
  "$probe_root/image-network/netavark.lock" \
  "$probe_root/network/cni.lock" \
  "$probe_root/network/netavark.lock"
chmod 0555 "$probe_root/empty-home" "$probe_root/empty-xdg"
chmod 0700 \
  "$probe_root/graphroot" \
  "$probe_image_backing" \
  "$probe_root/image-runtime" \
  "$probe_root/image-tmp" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chmod 0600 "$probe_root/image-network/cni.lock" "$probe_root/image-network/netavark.lock"
chmod 0600 "$probe_root/network/cni.lock" "$probe_root/network/netavark.lock"
chmod 0555 "$probe_root/image-network" "$probe_root/network"
chmod -R a-w "$probe_root/auth" "$probe_root/config" "$probe_root/hooks" "$probe_root/rootfs"

probe_install_unit_owned=1
/usr/bin/systemd-run \
  --wait \
  --collect \
  --pipe \
  --service-type=exec \
  --unit="${probe_install_unit%.service}" \
  --uid="$probe_user" \
  --property=KillMode=control-group \
  --property=CPUQuota=75% \
  --property=MemoryMax=100663296 \
  --property=MemorySwapMax=0 \
  --property=TasksMax=64 \
  /usr/bin/env -i \
  CONTAINERS_CONF="$probe_root/config/containers.conf" \
  CONTAINERS_REGISTRIES_CONF="$probe_root/config/registries.conf" \
  CONTAINERS_STORAGE_CONF="$probe_root/config/image-storage.conf" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=$probe_root/image-runtime/absent-user-bus" \
  GLAEDA_DISPOSABLE_PROBE=1 \
  HOME="$probe_root/empty-home" \
  LC_ALL=C \
  LOGNAME="$probe_user" \
  PATH=/usr/bin \
  PROBE_HOOKS="$probe_root/hooks" \
  PROBE_IMAGE_ID_RECORD="$probe_root/image-runtime/image.id" \
  PROBE_IMAGE_JSON="$probe_root/image-runtime/image.json" \
  PROBE_NETWORK="$probe_root/image-network" \
  PROBE_ROOTFS_TAR="$probe_root/rootfs.tar" \
  REGISTRY_AUTH_FILE="$probe_root/auth/auth.json" \
  TMPDIR="$probe_root/image-tmp" \
  USER="$probe_user" \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  XDG_RUNTIME_DIR="$probe_root/image-runtime" \
  /usr/bin/bash "$probe_user_script" --image-install-probe

install_cgroup="/system.slice/$probe_install_unit"
if [[ -e "/sys/fs/cgroup$install_cgroup" ]] || [[ -L "/sys/fs/cgroup$install_cgroup" ]] ||
  ! unit_load_state=$(/usr/bin/systemctl show --property=LoadState --value "$probe_install_unit" 2>/dev/null) ||
  [[ $unit_load_state != not-found ]] || /usr/bin/pgrep -u "$probe_uid" >/dev/null 2>&1; then
  printf 'error: offline image installation did not leave one collected empty service\n' >&2
  exit 1
fi
probe_install_unit_owned=0

image_id=$(< "$probe_root/image-runtime/image.id")
if [[ ! $image_id =~ ^sha256:[0-9a-f]{64}$ ]]; then
  printf 'error: offline image installation record is noncanonical\n' >&2
  exit 1
fi
printf '%s\n' "$image_id" > "$probe_root/config/image.id"
chmod 0444 "$probe_root/config/image.id"
/usr/bin/chown 0:0 "$probe_root/image-backing" "$probe_image_store"
/usr/bin/chmod 0700 "$probe_root/image-backing"
/usr/bin/chmod 0555 "$probe_image_store"
read -r backing_owner backing_group backing_mode < <(
  /usr/bin/stat -Lc '%u %g %a' "$probe_root/image-backing"
)
if [[ -L $probe_root/image-backing ]] || [[ ! -d $probe_root/image-backing ]] ||
  [[ $backing_owner != 0 ]] || [[ $backing_group != 0 ]] || [[ $backing_mode != 700 ]]; then
  printf 'error: writable image backing is not hidden behind one exact root-only parent\n' >&2
  exit 1
fi
image_store_identity=$(/usr/bin/stat -Lc '%d:%i' "$probe_image_backing")
probe_image_store_mounted=1
/usr/bin/mount --bind "$probe_image_backing" "$probe_image_store"
/usr/bin/mount --options remount,bind,ro,nosuid,nodev "$probe_image_store"
image_store_digest_before=$(snapshot_read_only_image_store "$probe_image_store" "$image_store_identity")
printf 'readonly_image_store=sealed\n'

/usr/bin/mount --types tmpfs \
  --options "rw,nosuid,nodev,size=8388608,nr_inodes=64,uid=$probe_uid,gid=$probe_gid,mode=0700" \
  smolrunner-target "$probe_target"
probe_target_mounted=1

probe_unit_owned=1
/usr/bin/systemd-run \
  --wait \
  --collect \
  --pipe \
  --service-type=exec \
  --unit="${probe_unit%.service}" \
  --uid="$probe_user" \
  --property=Delegate=yes \
  --property=KillMode=control-group \
  --property=CPUQuota=75% \
  --property=MemoryMax=100663296 \
  --property=MemorySwapMax=0 \
  --property=TasksMax=64 \
  /usr/bin/env -i \
  CONTAINERS_CONF="$probe_root/config/containers.conf" \
  CONTAINERS_REGISTRIES_CONF="$probe_root/config/registries.conf" \
  CONTAINERS_STORAGE_CONF="$probe_root/config/storage.conf" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=$probe_root/runtime/absent-user-bus" \
  GLAEDA_DISPOSABLE_PROBE=1 \
  HOME="$probe_root/empty-home" \
  LC_ALL=C \
  LOGNAME="$probe_user" \
  PATH=/usr/bin \
  PROBE_CGROUP_RECORD="$probe_root/runtime/service.cgroup" \
  PROBE_CIDFILE="$probe_root/runtime/container.cid" \
  PROBE_CAPTURE_STDERR="$probe_root/runtime/logs.stderr" \
  PROBE_CONTAINER_JSON="$probe_root/runtime/container.json" \
  PROBE_GRAPHROOT="$probe_root/graphroot" \
  PROBE_GROUP_SHA="$group_sha" \
  PROBE_HOOKS="$probe_root/hooks" \
  PROBE_HOSTILE_CIDFILE="$probe_root/runtime/hostile.cid" \
  PROBE_HOSTILE_CONTAINER_JSON="$probe_root/runtime/hostile-container.json" \
  PROBE_HOSTILE_INIT_STDERR="$probe_root/runtime/hostile-init.stderr" \
  PROBE_HOSTILE_INIT_STDOUT="$probe_root/runtime/hostile-init.stdout" \
  PROBE_HOSTILE_LOGFILE="$probe_root/runtime/hostile.log" \
  PROBE_IMAGE_ID_RECORD="$probe_root/config/image.id" \
  PROBE_IMAGE_JSON="$probe_root/runtime/image.json" \
  PROBE_IMAGE_STORE="$probe_image_store" \
  PROBE_INIT_STDERR="$probe_root/runtime/init.stderr" \
  PROBE_INIT_STDOUT="$probe_root/runtime/init.stdout" \
  PROBE_LOGFILE="$probe_root/runtime/container.log" \
  PROBE_NETWORK="$probe_root/network" \
  PROBE_PASSWD_SHA="$passwd_sha" \
  PROBE_ROOT="$probe_root" \
  PROBE_ROOTFS_TAR="$probe_root/rootfs.tar" \
  PROBE_RUNROOT="$probe_root/runtime/containers" \
  PROBE_SECCOMP_PROFILE="$probe_seccomp_profile" \
  PROBE_SECCOMP_SHA="$seccomp_sha" \
  PROBE_TARGET="$probe_target" \
  PROBE_GID="$probe_gid" \
  PROBE_UID="$probe_uid" \
  PROBE_USER="$probe_user" \
  REGISTRY_AUTH_FILE="$probe_root/auth/auth.json" \
  TMPDIR="$probe_root/tmp" \
  USER="$probe_user" \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  XDG_RUNTIME_DIR="$probe_root/runtime" \
  /usr/bin/bash "$probe_user_script" --user-probe
probe_unit_owned=0

image_store_digest_after=$(snapshot_read_only_image_store "$probe_image_store" "$image_store_identity")
if [[ $image_store_digest_after != "$image_store_digest_before" ]]; then
  printf 'error: read-only additional image store changed during container execution\n' >&2
  exit 1
fi
printf 'readonly_image_store=unchanged\n'
/usr/bin/umount -- "$probe_image_store"
probe_image_store_mounted=0

/usr/bin/umount -- "$probe_target"
probe_target_mounted=0

service_cgroup=$(< "$probe_root/runtime/service.cgroup")
if [[ $service_cgroup != "/system.slice/$probe_unit" ]] ||
  [[ -e "/sys/fs/cgroup$service_cgroup" ]] || [[ -L "/sys/fs/cgroup$service_cgroup" ]]; then
  printf 'error: disposable service cgroup identity remained after collection\n' >&2
  exit 1
fi

if /usr/bin/pgrep -u "$probe_uid" >/dev/null 2>&1; then
  printf 'error: offline container probe left an idle process\n' >&2
  exit 1
fi
if /usr/bin/findmnt -rn -o TARGET | /usr/bin/grep -Fq "$probe_root"; then
  printf 'error: offline container probe left a mount below its run-private root\n' >&2
  exit 1
fi

printf 'podman_container_closure_probe=pass\n'