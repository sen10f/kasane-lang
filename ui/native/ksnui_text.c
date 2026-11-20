// ksnui_text.c: libksnui の文字 (FreeType + HarfBuzz) 実装。ksnui_text.h の平坦な API を実現する。
#define _POSIX_C_SOURCE 200809L   /* strdup */
#include "ksnui_text.h"
#include <ft2build.h>
#include FT_FREETYPE_H
#include <hb.h>
#include <hb-ft.h>
#include <stdlib.h>
#include <string.h>

/* フォントのキャッシュ：同じ (path, size, face_index) なら FT_Face を使い回す。 */
typedef struct FontEntry {
    char* path;
    float size;
    uint32_t face_index;
    FT_Face face;
    hb_font_t* hb_font;
    struct FontEntry* next;
} FontEntry;

static FontEntry* font_list = NULL;

static FontEntry* find_font(const char* font_path, float size_px, uint32_t face_index) {
    FontEntry* f = font_list;
    while (f) {
        if (strcmp(f->path, font_path) == 0 && f->size == size_px && f->face_index == face_index) return f;
        f = f->next;
    }
    return NULL;
}

static FT_Library ft_lib = NULL;
static int ft_ready = 0;

static void ensure_ft(void) {
    if (!ft_ready) {
        if (FT_Init_FreeType(&ft_lib) != 0) {
            ft_ready = -1;
            return;
        }
        ft_ready = 1;
    }
}

static FontEntry* load_font(const char* font_path, float size_px, uint32_t face_index) {
    FontEntry* f = find_font(font_path, size_px, face_index);
    if (f) return f;
    ensure_ft();
    if (ft_ready <= 0) return NULL;
    f = (FontEntry*)calloc(1, sizeof(FontEntry));
    if (!f) return NULL;
    f->path = strdup(font_path);
    f->size = size_px;
    f->face_index = face_index;
    if (!f->path) { free(f); return NULL; }
    if (FT_New_Face(ft_lib, font_path, (FT_Int)face_index, &f->face) != 0) {
        free(f->path); free(f); return NULL;
    }
    FT_Set_Char_Size(f->face, 0, (FT_F26Dot6)(size_px * 64.0f), 0, 0);
    f->hb_font = hb_ft_font_create_referenced(f->face);
    if (!f->hb_font) {
        FT_Done_Face(f->face);
        free(f->path); free(f); return NULL;
    }
    f->next = font_list;
    font_list = f;
    return f;
}

size_t ksnui_text_shape(const char* font_path, float size_px, uint32_t face_index,
                        const char* utf8, ksnui_ShapedGlyph* out, size_t cap) {
    FontEntry* f = load_font(font_path, size_px, face_index);
    if (!f || !utf8 || !out || cap == 0) return 0;
    hb_buffer_t* buf = hb_buffer_create();
    hb_buffer_add_utf8(buf, utf8, -1, 0, -1);
    hb_buffer_guess_segment_properties(buf);
    hb_shape(f->hb_font, buf, NULL, 0);
    unsigned int glyph_count = 0;
    hb_glyph_info_t* info = hb_buffer_get_glyph_infos(buf, &glyph_count);
    hb_glyph_position_t* pos = hb_buffer_get_glyph_positions(buf, &glyph_count);
    size_t n = 0;
    for (unsigned int i = 0; i < glyph_count && n < cap; i++) {
        out[n].glyph_id = info[i].codepoint;
        out[n].cluster = info[i].cluster;
        out[n].x_advance = pos[i].x_advance / 64.0f;
        out[n].y_advance = pos[i].y_advance / 64.0f;
        out[n].x_offset = pos[i].x_offset / 64.0f;
        out[n].y_offset = pos[i].y_offset / 64.0f;
        n++;
    }
    hb_buffer_destroy(buf);
    return n;
}

int32_t ksnui_text_raster(const char* font_path, float size_px, uint32_t face_index, uint32_t glyph_id,
                          int32_t* out_w, int32_t* out_h,
                          int32_t* out_left, int32_t* out_top,
                          uint8_t* bitmap, size_t cap) {
    FontEntry* f = load_font(font_path, size_px, face_index);
    if (!f || !out_w || !out_h || !out_left || !out_top || !bitmap) return 1;
    if (FT_Load_Glyph(f->face, glyph_id, FT_LOAD_RENDER) != 0) return 1;
    FT_Bitmap* bm = &f->face->glyph->bitmap;
    *out_w = (int32_t)bm->width;
    *out_h = (int32_t)bm->rows;
    *out_left = (int32_t)f->face->glyph->bitmap_left;
    *out_top = (int32_t)f->face->glyph->bitmap_top;
    size_t bytes = (size_t)bm->width * (size_t)bm->rows;
    if (bytes > cap) return 1;
    for (size_t i = 0; i < (size_t)bm->rows; i++) {
        memcpy(bitmap + (size_t)i * (size_t)bm->width,
               bm->buffer + (size_t)i * (size_t)bm->pitch,
               (size_t)bm->width);
    }
    return 0;
}

float ksnui_text_line_height(const char* font_path, float size_px, uint32_t face_index) {
    FontEntry* f = load_font(font_path, size_px, face_index);
    if (!f) return size_px;
    return (float)f->face->height / 64.0f;
}
