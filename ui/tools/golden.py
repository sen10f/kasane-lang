#!/usr/bin/env python3
# ui/tools/golden.py: 2 枚の PNG を 1 チャネルごとの許容誤差と
# 最大相違画素数の予算で比較し、相違 PNG を書く。
# Pillow に依存せず zlib + PNG のフィルタ逆変換のみでデコードする。
# 終了コード: 0 一致 / 1 不一致 / 2 引数・形式エラー
import sys
import zlib
import struct

PNG_SIG = b"\x89PNG\r\n\x1a\n"


class PngError(ValueError):
    pass


def crc_chunk(chunk_type, data):
    return struct.pack(">I", zlib.crc32(chunk_type + data) & 0xFFFFFFFF)


def decode_png(path):
    with open(path, "rb") as f:
        buf = f.read()
    if buf[:8] != PNG_SIG:
        raise PngError(f"{path}: PNG のシグネチャが見つからない")
    pos = 8
    idat = bytearray()
    ihdr = None
    while pos < len(buf):
        if pos + 8 > len(buf):
            raise PngError(f"{path}: チャンクのヘッダが途切れている")
        (length,) = struct.unpack_from(">I", buf, pos)
        ctype = buf[pos + 4:pos + 8]
        pos += 8
        if pos + length + 4 > len(buf):
            raise PngError(f"{path}: チャンク {ctype!r} が途切れている")
        data = buf[pos:pos + length]
        pos += length + 4  # data + crc
        if ctype == b"IHDR":
            ihdr = data
        elif ctype == b"IDAT":
            idat += data
        elif ctype == b"IEND":
            break
    if ihdr is None:
        raise PngError(f"{path}: IHDR が見つからない")
    w, h, bit_depth, color_type, comp, filt, interlace = struct.unpack(">IIBBBBB", ihdr)
    if bit_depth != 8:
        raise PngError(f"{path}: ビット深度 8 以外は未対応 ({bit_depth})")
    if interlace != 0:
        raise PngError(f"{path}: インタレース 1 は未対応")
    if comp != 0 or filt != 0:
        raise PngError(f"{path}: 圧縮/フィルタ方式の組み合わせが非対応 ({comp}, {filt})")
    raw = zlib.decompress(bytes(idat))
    return _unfilter(raw, w, h, color_type), w, h


def _unfilter(raw, w, h, color_type):
    # 色形式 0(グレー),2(RGB),4(グレー+アルファ),6(RGBA) を RGBA に展開する。
    if color_type == 0:
        channels = 1
    elif color_type == 2:
        channels = 3
    elif color_type == 4:
        channels = 2
    elif color_type == 6:
        channels = 4
    else:
        raise PngError(f"色形式 {color_type} は未対応")
    bpp = channels
    stride = w * bpp
    out = bytearray(w * h * 4)
    prev = bytearray(stride)
    idx = 0
    for row in range(h):
        f = raw[idx]
        idx += 1
        line = bytearray(raw[idx:idx + stride])
        idx += stride
        if len(line) != stride:
            raise PngError("スキャンラインが途切れている")
        if f == 0:
            pass
        elif f == 1:
            for x in range(bpp, stride):
                line[x] = (line[x] + line[x - bpp]) & 0xFF
        elif f == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 0xFF
        elif f == 3:
            for x in range(stride):
                left = line[x - bpp] if x >= bpp else 0
                line[x] = (line[x] + ((prev[x] + left) // 2)) & 0xFF
        elif f == 4:
            for x in range(stride):
                left = line[x - bpp] if x >= bpp else 0
                up = prev[x]
                up_left = prev[x - bpp] if x >= bpp else 0
                p = left + up - up_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                pr = left if (pa <= pb and pa <= pc) else (up if pb <= pc else up_left)
                line[x] = (line[x] + pr) & 0xFF
        else:
            raise PngError(f"未知のフィルタ {f}")
        dst = row * w * 4
        if channels == 1:
            for x in range(w):
                v = line[x]
                out[dst + 4 * x] = v
                out[dst + 4 * x + 1] = v
                out[dst + 4 * x + 2] = v
                out[dst + 4 * x + 3] = 255
        elif channels == 2:
            for x in range(w):
                v = line[2 * x]
                out[dst + 4 * x] = v
                out[dst + 4 * x + 1] = v
                out[dst + 4 * x + 2] = v
                out[dst + 4 * x + 3] = line[2 * x + 1]
        elif channels == 3:
            for x in range(w):
                o = dst + 4 * x
                s = 3 * x
                out[o] = line[s]
                out[o + 1] = line[s + 1]
                out[o + 2] = line[s + 2]
                out[o + 3] = 255
        else:
            out[dst:dst + w * 4] = line
        prev = line
    return bytes(out), w, h


def encode_png_rgba(rgba, w, h):
    rows = bytearray()
    for y in range(h):
        rows.append(0)
        rows += rgba[y * w * 4:(y + 1) * w * 4]
    compressed = zlib.compress(bytes(rows), 6)
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    out = bytearray(PNG_SIG)
    out += struct.pack(">I", len(ihdr)) + b"IHDR" + ihdr + crc_chunk(b"IHDR", ihdr)
    out += struct.pack(">I", len(compressed)) + b"IDAT" + compressed + crc_chunk(b"IDAT", compressed)
    iend = b""
    out += struct.pack(">I", 0) + b"IEND" + crc_chunk(b"IEND", iend)
    return bytes(out)


def compare(actual, expected, tol, max_px):
    """実際の画像と期待画像を比べる。(相違画素数, 最大チャネル差)"""
    apx, aw, ah = actual
    epx, ew, eh = expected
    if aw != ew or ah != eh:
        return None, None, ("サイズが違う: actual %dx%d expected %dx%d" % (aw, ah, ew, eh))
    differ = 0
    max_ch = 0
    n = aw * ah * 4
    for i in range(0, n, 4):
        d0 = abs(apx[i] - epx[i])
        d1 = abs(apx[i + 1] - epx[i + 1])
        d2 = abs(apx[i + 2] - epx[i + 2])
        d3 = abs(apx[i + 3] - epx[i + 3])
        d = d0
        if d1 > d:
            d = d1
        if d2 > d:
            d = d2
        if d3 > d:
            d = d3
        if d > max_ch:
            max_ch = d
        if d0 > tol or d1 > tol or d2 > tol or d3 > tol:
            differ += 1
    return differ, max_ch, None


def make_diff_png(actual, expected, diff_path, tol):
    apx, aw, ah = actual
    epx, _, _ = expected
    out = bytearray()
    n = aw * ah * 4
    size_ok = (len(apx) == len(epx))
    for i in range(0, n, 4):
        if not size_ok:
            out += apx[i:i + 4]
            continue
        d0 = abs(apx[i] - epx[i])
        d1 = abs(apx[i + 1] - epx[i + 1])
        d2 = abs(apx[i + 2] - epx[i + 2])
        d3 = abs(apx[i + 3] - epx[i + 3])
        if d0 > tol or d1 > tol or d2 > tol or d3 > tol:
            out += b"\xff\x00\x00" + bytes((apx[i + 3],))  # 相違: 赤 (元のアルファ)
        else:
            out += apx[i:i + 4]
    with open(diff_path, "wb") as f:
        f.write(encode_png_rgba(bytes(out), aw, ah))


def main(argv):
    if len(argv) < 4:
        sys.stderr.write(
            "usage: golden.py <actual.png> <expected.png> <diff.png> "
            "[--tol N] [--max-px N]\n")
        return 2
    actual_path, expected_path, diff_path = argv[1], argv[2], argv[3]
    tol = 4
    max_px = 0
    i = 4
    while i < len(argv):
        if argv[i] == "--tol" and i + 1 < len(argv):
            tol = int(argv[i + 1])
            i += 2
        elif argv[i] == "--max-px" and i + 1 < len(argv):
            max_px = int(argv[i + 1])
            i += 2
        else:
            sys.stderr.write("golden.py: 不明な引数 %s\n" % argv[i])
            return 2
    try:
        actual, aw, ah = decode_png(actual_path)
        expected, ew, eh = decode_png(expected_path)
    except (PngError, OSError, zlib.error) as e:
        sys.stderr.write("golden.py: %s\n" % e)
        return 2
    differ, max_ch, err = compare((actual, aw, ah), (expected, ew, eh), tol, max_px)
    if err is not None:
        sys.stderr.write("golden.py: MISMATCH %s\n" % err)
        # 差分 PNG には actual をそのまま書き出す (何を比べたか分かるように)
        try:
            make_diff_png((actual, aw, ah), (expected, ew, eh), diff_path, tol)
        except OSError as e:
            sys.stderr.write("golden.py: 差分 PNG の書き出しに失敗: %s\n" % e)
        return 1
    over = differ > max_px
    try:
        make_diff_png((actual, aw, ah), (expected, ew, eh), diff_path, tol)
    except OSError as e:
        sys.stderr.write("golden.py: 差分 PNG の書き出しに失敗: %s\n" % e)
        return 2
    if over:
        sys.stderr.write(
            "golden.py: MISMATCH 相違画素 %d (予算 %d) 最大チャネル差 %d\n"
            % (differ, max_px, max_ch))
        return 1
    sys.stdout.write(
        "golden.py: OK 相違画素 %d (予算 %d) 最大チャネル差 %d 許容誤差 %d\n"
        % (differ, max_px, max_ch, tol))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
