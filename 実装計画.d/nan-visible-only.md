# kir/nan-visible-only (2026-11-23)
NaN の正規化を演算ごとから、ビットが見える所 (to_bits・メモリへの書き出し・欄・extern 引数) だけに移した (canon_i、整数ビットで判定)。旧試験 3 本の期待は正規の NaN に改めた。f32 内積 約900ms→約145ms (C -O2 約117ms)。
