// ksnui_text.h: libksnui の文字 (FreeType + HarfBuzz) の平坦な API。ksnui.h から取り込まれる。
// 方針 (設計_UIの枠組み.md 2.3): UTF-8 の文字列を HarfBuzz で成形してグリフの位置を出し、
// FreeType で個々のグリフを 8 bit 灰色にラスタ化する。atlas への配置は Kasane 側が決める。
#ifndef KSNUI_TEXT_H
#define KSNUI_TEXT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 成形したグリフ 1 個。cluster は UTF-8 文字列内のバイトオフセット。
typedef struct ksnui_ShapedGlyph {
    uint32_t glyph_id;  // FreeType のグリフ索引
    uint32_t cluster;   // cluster (UTF-8 のバイトオフセット)
    float x_advance;    // 次のグリフへの進み (px)
    float y_advance;
    float x_offset;     // 描画位置からのずれ (px)
    float y_offset;
} ksnui_ShapedGlyph;

// utf8 (NUL 終端) を font_path の face_index 番のフェイスを size_px で成形し、
// 最大 cap 個まで out に書く。返り値は書いたグリフの数。
size_t ksnui_text_shape(const char* font_path, float size_px, uint32_t face_index,
                        const char* utf8, ksnui_ShapedGlyph* out, size_t cap);

// グリフ 1 個を 8 bit 灰色でラスタ化する。bitmap (cap バイト) に行は w バイト詰めで書く。
// left / top はベースラインからの描画位置のずれ (px)。成功で 0、失敗で 0 以外。
int32_t ksnui_text_raster(const char* path, float size_px, uint32_t face_index, uint32_t glyph_id,
                          int32_t* w, int32_t* h, int32_t* left, int32_t* top,
                          uint8_t* buf, size_t cap);

// 行の高さ (px)。
float ksnui_text_line_height(const char* font_path, float size_px, uint32_t face_index);

#ifdef __cplusplus
}
#endif

#endif // KSNUI_TEXT_H
