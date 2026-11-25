#!/usr/bin/env python3
"""Turns a coded stream into v4l2-ctl's --stream-from-hdr format: one access unit per buffer.

  v4l2-hdr.py h264|hevc IN.annexb OUT.hdr   Annex B with access unit delimiters (x264/x265 aud=1)
  v4l2-hdr.py ivf IN.ivf OUT.hdr            VP8/VP9 in IVF, one frame per buffer

Each buffer is written as big-endian 'Vhdr' (v4l2_fourcc), its length, then the bytes
(v4l2-ctl-streaming.cpp, fill_buffer_from_file).
"""
import struct
import sys

HDR_ID = ord('V') | ord('h') << 8 | ord('d') << 16 | ord('r') << 24


def annexb_units(data, is_aud):
    starts = []
    i = 0
    while True:
        i = data.find(b'\x00\x00\x01', i)
        if i < 0:
            break
        if is_aud(data[i + 3]):
            # Keep the leading zero of a 4-byte start code with its unit.
            starts.append(i - 1 if i > 0 and data[i - 1] == 0 else i)
        i += 3
    starts.append(len(data))
    if starts[0] != 0:
        starts.insert(0, 0)
    return [data[a:b] for a, b in zip(starts, starts[1:]) if b > a]


def ivf_frames(data):
    hdr_len = struct.unpack_from('<H', data, 6)[0]
    pos = hdr_len
    while pos + 12 <= len(data):
        size = struct.unpack_from('<I', data, pos)[0]
        yield data[pos + 12:pos + 12 + size]
        pos += 12 + size


def main():
    kind, src, dst = sys.argv[1:4]
    data = open(src, 'rb').read()
    if kind == 'h264':
        units = annexb_units(data, lambda b: b & 0x1f == 9)
    elif kind == 'hevc':
        units = annexb_units(data, lambda b: (b >> 1) & 0x3f == 35)
    else:
        units = list(ivf_frames(data))
    with open(dst, 'wb') as out:
        for u in units:
            out.write(struct.pack('>II', HDR_ID, len(u)))
            out.write(u)
    print(f'{dst}: {len(units)} buffers')


if __name__ == '__main__':
    main()
