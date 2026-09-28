#!/bin/bash
# Print the Xcode build settings that route compilation caching through the
# node daemon, one per line, but only if the daemon answers on its socket.
# With the socket missing, Xcode 26.3 neither fails nor falls back quickly:
# a 17 s build took about 400 s. So callers add these settings only when this
# prints them, and otherwise build with the local cache alone.
#
# usage: fleet-cas-settings.sh <socket>   (exit 0 = settings printed, 1 = node down)
set -u
sock=${1:?usage: fleet-cas-settings.sh <socket>}
node_up() {
  [ -S "$1" ] && perl -MIO::Socket::UNIX -e '
    local $SIG{ALRM} = sub { exit 1 }; alarm 2;
    IO::Socket::UNIX->new(Type => SOCK_STREAM(), Peer => $ARGV[0]) or exit 1' "$1"
}
if node_up "$sock"; then
  printf '%s\n' COMPILATION_CACHE_ENABLE_PLUGIN=YES "COMPILATION_CACHE_REMOTE_SERVICE_PATH=$sock"
else
  echo "fleet-cas node not answering on $sock; building with the local cache only" >&2
  exit 1
fi
