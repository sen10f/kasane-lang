// ksnui_gpu.h: libksnui の GPU (wgpu-native) の平坦な API。ksnui.h から取り込まれる。
// 方針 (設計_UIの枠組み.md 2.2): 装置の作成は同期風に包み、1 フレームの描画は
// 角丸四角の列とグリフの列の 2 系統。window が NULL なら headless (画面なしの経路)。
#ifndef KSNUI_GPU_H
#define KSNUI_GPU_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 角丸四角 1 枚。x, y, w, h は論理 px (左上原点)、r は角の半径、色は 0..1 の RGBA。
typedef struct ksnui_Rect {
    float x, y, w, h;
    float r;
    float cr, cg, cb, ca;
} ksnui_Rect;

// グリフ 1 枚の四角。x, y, w, h は描画先の位置と大きさ (論理 px)、
// u0, v0, u1, v1 は atlas 内の UV (0..1)、色は 0..1 の RGBA。
typedef struct ksnui_GlyphQuad {
    float x, y, w, h;
    float u0, v0, u1, v1;
    float cr, cg, cb, ca;
} ksnui_GlyphQuad;

// GPU の装置を作る。window が NULL なら headless (offscreen texture の経路)。
// w, h は描画先の大きさ (論理 px)。失敗したら NULL。
void* ksnui_gpu_create(void* window, int32_t w, int32_t h);

// 装置を解放する。
void ksnui_gpu_destroy(void* dev);

// 描画先の大きさを変える (headless の texture の作り直し、または surface の再設定)。
int32_t ksnui_gpu_resize(void* dev, int32_t w, int32_t h);

// 1 フレームを始め、背景を (r, g, b, a) で塗りつぶす準備をする。失敗したら 0 以外。
int32_t ksnui_gpu_begin(void* dev, float r, float g, float b, float a);

// 角丸四角を n 枚描く (begin と end の間に呼ぶ)。
void ksnui_gpu_draw_rects(void* dev, const ksnui_Rect* rects, size_t n);

// グリフ atlas を uploading する。pixels は w*h バイトの 8 bit 灰色 (R8、行は w バイト詰め)、
// atlas の原点 (0, 0) の w x h の領域に書く。atlas は 1024x1024。
void ksnui_gpu_atlas_upload(void* dev, int32_t w, int32_t h, const uint8_t* pixels);

// グリフを n 枚描く (atlas_upload の後、begin と end の間に呼ぶ)。
void ksnui_gpu_draw_glyphs(void* dev, const ksnui_GlyphQuad* quads, size_t n);

// 1 フレームを閉じて提出する。window があれば present する。
void ksnui_gpu_end(void* dev);

// 描画先を RGBA8 (w*h*4 バイト、行は w*4 バイト詰め) で読み戻す。
// out に書いたバイト数を返す。cap が足りなければ 0。
size_t ksnui_gpu_readback(void* dev, uint8_t* out, size_t cap);

// RGBA8 の画像を PNG (8 bit RGBA、無圧縮 deflate) で書く。成功で 0。
int32_t ksnui_png_write(const char* path, int32_t w, int32_t h, const uint8_t* rgba);

#ifdef __cplusplus
}
#endif

#endif // KSNUI_GPU_H
