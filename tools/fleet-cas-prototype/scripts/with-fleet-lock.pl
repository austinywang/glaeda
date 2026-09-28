#!/usr/bin/perl
# Hold a cmux build fleet mini's host lock while running one command, so the
# launchd build worker cannot start a job during a measurement. Unlike the
# fleet's own with-host-lock, it does not apply the job disk floor and gives
# up after 20 minutes instead of waiting forever.
use strict;
use warnings;
use Fcntl qw(:flock);
@ARGV or die "usage: with-fleet-lock.pl COMMAND [ARG...]\n";
my $path = $ENV{FLEET_HOST_LOCK} // '/Users/Shared/cmux-build-fleet/host.lock';
open(my $lock, '+<', $path) or die "open $path: $!\n";
if (!flock($lock, LOCK_EX | LOCK_NB)) {
    print STDERR "waiting for the current fleet job\n";
    local $SIG{ALRM} = sub { die "fleet lock still held after 20 minutes\n" };
    alarm 1200;
    flock($lock, LOCK_EX) or die "lock: $!\n";
    alarm 0;
}
system { $ARGV[0] } @ARGV;
exit($? == -1 ? 127 : ($? & 127) ? 128 + ($? & 127) : $? >> 8);
