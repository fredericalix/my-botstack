#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Build the P4 board's Wi-Fi partition image from wifi.txt (for flash.sh).

    wifi_nvs.py wifi.txt partitions.csv out.bin

wifi.txt: one network per line, ssid<TAB>password; "ssid<TAB>" alone is an open
network. Only a trailing CR is dropped (a password may end with a space). Blank
lines and lines whose first non-blank character is # are skipped. A line that
is not ssid<TAB>password, an SSID over 32 bytes, a password over 64 bytes, or a
network after the tenth is skipped with a warning naming its line number only,
never its content (it may hold a password).

The image is NVS namespace "wifi", keys ssid/password, ssid1/password1 ...
ssid9/password9 (the StackChan firmware's format), sized for the `wifi_nvs`
partition. It is built in memory: no CSV is written. Prints the partition
offset on stdout and the counts on stderr; fails when no line is valid.
"""
import contextlib
import io
import os
import sys

from esp_idf_nvs_partition_gen import nvs_partition_gen as gen

PARTITION = 'wifi_nvs'
MAX_NETWORKS = 10
MAX_SSID = 32
MAX_PASSWORD = 64


def fail(message):
    sys.exit('wifi_nvs: ' + message)


def partition(path):
    """Offset and size of the wifi_nvs partition in partitions.csv."""
    with open(path) as f:
        for raw in f:
            line = raw.split('#', 1)[0].strip()
            if not line:
                continue
            fields = [field.strip() for field in line.split(',')]
            if fields[0] != PARTITION:
                continue
            if len(fields) < 5 or fields[1:3] != ['data', 'nvs'] or not fields[3] or not fields[4]:
                fail('%s in %s must be "data, nvs" with an explicit offset and size' % (PARTITION, path))
            return int(fields[3], 0), int(fields[4], 0)
    fail('no %s partition in %s' % (PARTITION, path))


def networks(path):
    """The valid networks of wifi.txt, as (ssid, password) strings."""
    with open(path, 'rb') as f:
        data = f.read()
    kept = []
    skipped = []
    for number, line in enumerate(data.split(b'\n'), 1):
        if line.endswith(b'\r'):
            line = line[:-1]
        first = line.lstrip(b' \t')
        if not first or first.startswith(b'#'):
            continue
        tab = line.find(b'\t')
        why = None
        if tab <= 0:
            why = 'it is not ssid<TAB>password'
        else:
            ssid, password = line[:tab], line[tab + 1:]
            if len(ssid) > MAX_SSID:
                why = 'the SSID is longer than %d bytes' % MAX_SSID
            elif len(password) > MAX_PASSWORD:
                why = 'the password is longer than %d bytes' % MAX_PASSWORD
            elif b'\0' in line:
                why = 'it holds a NUL byte'
            elif len(kept) == MAX_NETWORKS:
                why = 'only %d networks fit' % MAX_NETWORKS
            else:
                try:
                    kept.append((ssid.decode('utf-8'), password.decode('utf-8')))
                except UnicodeDecodeError:
                    why = 'it is not UTF-8'
        if why:
            skipped.append(number)
            print('wifi.txt line %d skipped: %s' % (number, why), file=sys.stderr)
    return kept, skipped


def main():
    if len(sys.argv) != 4:
        fail('usage: wifi_nvs.py wifi.txt partitions.csv out.bin')
    source, table, output = sys.argv[1:]
    offset, size = partition(table)
    kept, skipped = networks(source)
    print('wifi.txt: %d network(s) kept, %d line(s) skipped' % (len(kept), len(skipped)), file=sys.stderr)
    if not kept:
        fail('no valid network in wifi.txt (ssid<TAB>password, the tab is required)')

    usable, _ = gen.check_size(hex(size))  # one page stays reserved
    image = io.BytesIO()
    # The generator's own messages go to stderr: stdout is the offset only.
    with contextlib.redirect_stdout(sys.stderr), gen.nvs_open(image, usable, gen.Page.VERSION2) as nvs:
        gen.write_entry(nvs, 'wifi', 'namespace', '', None)
        for i, (ssid, password) in enumerate(kept):
            suffix = str(i) if i else ''
            gen.write_entry(nvs, 'ssid' + suffix, 'data', 'string', ssid)
            gen.write_entry(nvs, 'password' + suffix, 'data', 'string', password)
    data = image.getvalue()
    if len(data) != size:
        fail('the image is %d bytes, the partition %d' % (len(data), size))

    # Only the owner may read the image: it holds the passwords.
    fd = os.open(output + '.tmp', os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'wb') as f:
        f.write(data)
    os.replace(output + '.tmp', output)
    print(hex(offset))


if __name__ == '__main__':
    main()
