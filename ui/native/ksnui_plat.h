// ksnui_plat.h: libksnui の窓・入力 (SDL3) の平坦な API。ksnui.h から取り込まれる。
// 方針 (設計_UIの枠組み.md 2.1): スカラーとポインタと平坦な構造体だけ。構造体の値渡しに頼らず、
// 固定長配列を構造体に入れない (ネイティブ生成の対象外、仕様 3.8)。文字列は text_pool にまとめて書く。
#ifndef KSNUI_PLAT_H
#define KSNUI_PLAT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 事象の種類。ksnui_Event.type に入る。
enum ksnui_EventType {
    KSNUI_EVENT_NONE = 0,
    KSNUI_EVENT_QUIT = 1,             // 窓を閉じる
    KSNUI_EVENT_KEY_DOWN = 2,         // key に SDL_Keycode
    KSNUI_EVENT_KEY_UP = 3,
    KSNUI_EVENT_POINTER_DOWN = 4,     // x, y は論理 px
    KSNUI_EVENT_POINTER_UP = 5,
    KSNUI_EVENT_POINTER_MOVE = 6,
    KSNUI_EVENT_WHEEL = 7,            // wheel_x, wheel_y
    KSNUI_EVENT_RESIZE = 8,           // w, h は新しい大きさ
    KSNUI_EVENT_TEXT_INPUT = 9,       // 確定した文字列 (text_off/text_len)
    KSNUI_EVENT_IME_START = 10,       // 変換 (IME) の開始
    KSNUI_EVENT_IME_COMPOSITION = 11, // 変換中の文字列。cursor はカーソルのバイト位置
    KSNUI_EVENT_IME_COMMIT = 12,      // 変換の確定文字列
    KSNUI_EVENT_IME_END = 13          // 変換の終了
};

// 飲み込み (polling) で取る事象 1 件。SDL の事象を写しただけの平坦な構造体。
typedef struct ksnui_Event {
    int32_t event_type; // enum ksnui_EventType
    int32_t key;       // KEY_DOWN / KEY_UP のとき SDL_Keycode、他は 0
    int32_t mods;      // 修飾キー (SDL_Keymod の下位 16 bit)
    float x;           // POINTER_* のときの位置 (論理 px)
    float y;
    float wheel_x;     // WHEEL のときの移動量
    float wheel_y;
    int32_t w;         // RESIZE のときの新しい幅
    int32_t h;         // RESIZE のときの新しい高さ
    int32_t cursor;    // IME_COMPOSITION のときのカーソル (バイト位置)
    uint32_t text_off; // text_pool 内の UTF-8 のオフセット (なければ 0)
    uint32_t text_len; // text_pool 内の UTF-8 のバイト長
} ksnui_Event;

// 窓を作る。失敗したら NULL。w, h は論理 px。
void* ksnui_window_create(const char* title, int32_t w, int32_t h);

// 窓を閉じて資源を解放する。
void ksnui_window_destroy(void* win);

// 滞っている事象を最大 cap 件まで out に書き、text_pool (text_pool_cap バイト) に
// UTF-8 の文字列をまとめて書く。返り値は書いた事象の数。
size_t ksnui_poll_events(void* win, ksnui_Event* out, size_t cap, char* text_pool, size_t text_pool_cap);

// IME の変換を開始し、変換候補の窓の位置を (x, y, w, h) にする (論理 px)。
void ksnui_text_input_start(void* win, int32_t x, int32_t y, int32_t w, int32_t h);

// IME の変換をやめる。
void ksnui_text_input_stop(void* win);

// 表示の縮尺 (SDL_GetWindowDisplayScale)。論理 px への換算に使う。
float ksnui_display_scale(void* win);

// クリップボード。text は UTF-8。得られた長さを返す (終端の '\0' は含まず)。
size_t ksnui_clipboard_get(void* win, char* buf, size_t cap);
bool ksnui_clipboard_set(void* win, const char* text);

// カーソルの形状。shape は SDL_SystemCursor の値。
bool ksnui_cursor_set(void* win, int32_t shape);

// クリップボード。text は UTF-8。得られた長さを返す (終端の '\0' は含まず)。
size_t ksnui_clipboard_get(void* win, char* buf, size_t cap);
bool ksnui_clipboard_set(void* win, const char* text);

// カーソルの形状。shape は SDL_SystemCursor の値。
bool ksnui_cursor_set(void* win, int32_t shape);

#ifdef __cplusplus
}
#endif

#endif // KSNUI_PLAT_H
