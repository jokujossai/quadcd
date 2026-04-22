#!/bin/sh
# Fake command for integration tests.
#
# Controlled via environment variables:
#   FAKE_EXIT_CODE  — exit code (default: 0)
#   FAKE_STDOUT     — if set, print this instead of the args
#   FAKE_STDERR     — if set, print this to stderr instead of the args
#   FAKE_SLEEP      — if set, sleep this many seconds before running

if [ -n "$FAKE_SLEEP" ]; then
    sleep "$FAKE_SLEEP"
fi

if [ -n "$FAKE_STDOUT" ]; then
    echo "$FAKE_STDOUT"
else
    echo "$@"
fi

if [ -n "$FAKE_STDERR" ]; then
    echo "$FAKE_STDERR" >&2
else
    echo "$@" >&2
fi

exit "${FAKE_EXIT_CODE:-0}"
