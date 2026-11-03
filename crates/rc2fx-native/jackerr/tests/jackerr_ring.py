#!/usr/bin/env python3
"""Decode a snapshot of the jackd error ring.

    jackerr_ring.py SNAPSHOT

SNAPSHOT is written ON THE DEVICE by rc2fx_jackerr_snapshot (in the parent
directory), which reads each live slot with its sequence lock. The tests'
reference decoder; `cargo xtask mixer xruns` decodes the same format.
This decoder never reads a live ring file: a raw copy cannot show whether a
slot changed while it was copied. Prints JSON: the ring's counters as read
and every stable record, oldest first.
"""
import json
import struct
import sys

MAGIC = b'RC2FXJS1'
HEADER = struct.Struct('<8sII' + 'Q' * 14)  # 128 bytes
RECORD = struct.Struct('<QQQqq40s16s')
KINDS = {1: 'clientNotFinished', 2: 'clientFinishedLate', 3: 'timedDriverXrun', 4: 'alsaXrun',
         5: 'alsaWaitStatus', 6: 'internalXrun', 7: 'pollFailed', 8: 'pollTimeoutExiting',
         9: 'pollTimeoutRetrying', 10: 'pollRecoveryFailed', 11: 'pollInterrupt', 255: 'unknownFormat'}
FLAGS = {1: 'string1Truncated', 2: 'string2Truncated', 4: 'string1Null', 8: 'string2Null', 16: 'clockFailed'}
VALUES = {'timedDriverXrun': ('usecs',), 'alsaXrun': ('usecs',), 'alsaWaitStatus': ('status',),
          'internalXrun': ('frameOffset',), 'pollTimeoutExiting': ('usecs', 'retries'),
          'pollTimeoutRetrying': ('usecs', 'retries'), 'pollRecoveryFailed': ('status',)}
STRINGS = {'clientNotFinished': ('client', 'state'), 'clientFinishedLate': ('client',),
           'pollFailed': ('reason',), 'unknownFormat': ('format', 'formatContinued')}


class RingError(Exception):
    pass


def read(data):
    if len(data) < HEADER.size or data[:8] != MAGIC:
        raise RingError('not an RC2FX jackd error ring snapshot')
    fields = HEADER.unpack_from(data)
    (_, version, record_size, pid, created, claimed, dropped, unknown, calls, snapshot_ns,
     first_index, end_index, stable, unpublished, overwritten, torn, _) = fields
    if version != 1 or record_size != RECORD.size:
        raise RingError(f'unsupported snapshot version {version} / record size {record_size}')
    if len(data) != HEADER.size + stable * RECORD.size:
        raise RingError('snapshot length does not match its record count')
    if stable + unpublished + overwritten + torn != end_index - first_index:
        raise RingError('snapshot accounting does not add up')
    records = []
    for offset in range(HEADER.size, len(data), RECORD.size):
        index, t, kind_flags, value1, value2, string1, string2 = RECORD.unpack_from(data, offset)
        kind = KINDS.get(kind_flags & 0xffff, kind_flags & 0xffff)
        flags = [name for bit, name in FLAGS.items() if (kind_flags >> 16) & bit]
        lengths = ((kind_flags >> 32) & 0xff, (kind_flags >> 40) & 0xff)
        entry = {'index': index, 'tNs': None if 'clockFailed' in flags else t, 'kind': kind}
        for name, value in zip(VALUES.get(kind, ()), (value1, value2)):
            entry[name] = value
        for position, (name, raw) in enumerate(zip(STRINGS.get(kind, ()), (string1, string2))):
            null = ('string1Null', 'string2Null')[position] in flags
            entry[name] = None if null else raw[:lengths[position]].decode('utf-8', 'replace')
        if flags:
            entry['flags'] = flags
        records.append(entry)
    # The reader writes 0 when it could not read the clock.
    return {'pid': pid, 'createdNs': created, 'snapshotNs': snapshot_ns or None,
            'snapshotClockValid': snapshot_ns != 0, 'calls': calls,
            'unknownFormats': unknown, 'claimed': claimed, 'dropped': dropped,
            'window': {'firstIndex': first_index, 'endIndex': end_index},
            'stable': stable, 'unpublished': unpublished, 'overwritten': overwritten, 'torn': torn,
            'overwrittenBeforeWindow': first_index, 'records': records}


def main():
    try:
        print(json.dumps(read(open(sys.argv[1], 'rb').read()), indent=2))
    except (RingError, OSError, struct.error) as error:
        print(f'jackerr_ring: {error}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
