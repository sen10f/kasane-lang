// ksnui.h: Kasane のネイティブ UI の C の橋 (libksnui) の見出し。
// SDL3 (窓・入力) / wgpu-native (GPU) / FreeType + HarfBuzz (文字) を平坦な C の API に包む。
// 使うのはスカラー・ポインタ・平坦な構造体だけ。コールバックは user data なし。
// 各層の宣言は ksnui_plat.h / ksnui_gpu.h / ksnui_text.h に分かれていて、ここから取り込む
// (ksn bindgen --c は各層の見出しから ui.plat / ui.gpu / ui.text の宣言を生成する)。
#ifndef KSNUI_H
#define KSNUI_H

#include "ksnui_plat.h"
#include "ksnui_gpu.h"
#include "ksnui_text.h"

#endif // KSNUI_H
