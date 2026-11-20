// ksnui_plat.c: libksnui の窓・入力 (SDL3) 実装。ksnui_plat.h の平坦な API を実現する。
#include "ksnui_plat.h"
#include <SDL3/SDL.h>
#include <stdlib.h>
#include <string.h>

/* SDL の初期化状態。複数回呼んでも安全。 */
static int sdl_initialized = 0;

static void ensure_sdl(void) {
    if (!sdl_initialized) {
        if (!SDL_Init(SDL_INIT_VIDEO)) {
            sdl_initialized = -1;
        } else {
            sdl_initialized = 1;
        }
    }
}

void* ksnui_window_create(const char* title, int32_t w, int32_t h) {
    ensure_sdl();
    if (sdl_initialized <= 0) return NULL;
    SDL_Window* win = SDL_CreateWindow(title, (int)w, (int)h,
                                       SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIDDEN);
    return (void*)win;
}

void ksnui_window_destroy(void* win) {
    if (!win) return;
    SDL_DestroyWindow((SDL_Window*)win);
}

size_t ksnui_poll_events(void* win, ksnui_Event* out, size_t cap,
                         char* text_pool, size_t text_pool_cap) {
    (void)win;
    size_t n = 0;
    size_t pool_off = 0;
    SDL_Event ev;
    while (n < cap && SDL_PollEvent(&ev)) {
        ksnui_Event* e = &out[n];
        memset(e, 0, sizeof(*e));
        switch (ev.type) {
        case SDL_EVENT_QUIT:
            e->type = KSNUI_EVENT_QUIT;
            break;
        case SDL_EVENT_KEY_DOWN:
            e->type = KSNUI_EVENT_KEY_DOWN;
            e->key = (int32_t)ev.key.key;
            e->mods = (int32_t)ev.key.mod;
            break;
        case SDL_EVENT_KEY_UP:
            e->type = KSNUI_EVENT_KEY_UP;
            e->key = (int32_t)ev.key.key;
            e->mods = (int32_t)ev.key.mod;
            break;
        case SDL_EVENT_MOUSE_BUTTON_DOWN:
            e->type = KSNUI_EVENT_POINTER_DOWN;
            e->x = ev.button.x;
            e->y = ev.button.y;
            e->key = (int32_t)ev.button.button;
            break;
        case SDL_EVENT_MOUSE_BUTTON_UP:
            e->type = KSNUI_EVENT_POINTER_UP;
            e->x = ev.button.x;
            e->y = ev.button.y;
            e->key = (int32_t)ev.button.button;
            break;
        case SDL_EVENT_MOUSE_MOTION:
            e->type = KSNUI_EVENT_POINTER_MOVE;
            e->x = ev.motion.x;
            e->y = ev.motion.y;
            break;
        case SDL_EVENT_MOUSE_WHEEL:
            e->type = KSNUI_EVENT_WHEEL;
            e->wheel_x = ev.wheel.x;
            e->wheel_y = ev.wheel.y;
            break;
        case SDL_EVENT_WINDOW_RESIZED:
            e->type = KSNUI_EVENT_RESIZE;
            e->w = ev.window.data1;
            e->h = ev.window.data2;
            break;
        case SDL_EVENT_TEXT_INPUT: {
            size_t len = strlen(ev.text.text);
            if (pool_off + len + 1 <= text_pool_cap) {
                memcpy(text_pool + pool_off, ev.text.text, len + 1);
                e->type = KSNUI_EVENT_TEXT_INPUT;
                e->text_off = (uint32_t)pool_off;
                e->text_len = (uint32_t)len;
                pool_off += len + 1;
            }
            break;
        }
        case SDL_EVENT_TEXT_EDITING: {
            const SDL_TextEditingEvent* ed = &ev.edit;
            size_t len = strlen(ed->text);
            if (pool_off + len + 1 <= text_pool_cap) {
                memcpy(text_pool + pool_off, ed->text, len + 1);
                e->type = KSNUI_EVENT_IME_COMPOSITION;
                e->text_off = (uint32_t)pool_off;
                e->text_len = (uint32_t)len;
                e->cursor = (int32_t)ed->start;
                pool_off += len + 1;
            }
            break;
        }
        default:
            break;
        }
        if (e->type != 0 || ev.type == SDL_EVENT_TEXT_INPUT ||
            ev.type == SDL_EVENT_TEXT_EDITING) {
            n++;
        }
    }
    return n;
}

void ksnui_text_input_start(void* win, int32_t x, int32_t y, int32_t w, int32_t h) {
    /* SDL3: SDL_Rect は int ベース。SDL_SetTextInputArea で変換領域を指定する。 */
    SDL_Rect rect;
    rect.x = x; rect.y = y;
    rect.w = w; rect.h = h;
    SDL_SetTextInputArea((SDL_Window*)win, &rect, 0);
    SDL_StartTextInput((SDL_Window*)win);
}

void ksnui_text_input_stop(void* win) {
    SDL_StopTextInput((SDL_Window*)win);
}

float ksnui_display_scale(void* win) {
    /* SDL_GetWindowDisplayScale は 1 つの float (表示の縮尺) を返す。 */
    float scale = SDL_GetWindowDisplayScale((SDL_Window*)win);
    if (scale > 0.0f) return scale;
    return 1.0f;
}
