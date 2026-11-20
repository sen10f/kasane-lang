// Z2 smoke test (native build): headless device, rounded rect, readback, PNG, pixel checksum.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "ksnui_gpu.h"

#define W 160
#define H 120

static uint32_t checksum(const uint8_t* rgba, size_t n) {
    uint32_t c = 0;
    for (size_t i = 0; i < n; i++) c = c * 31u + (uint32_t)rgba[i];
    return c;
}

int main(void) {
    void* dev = ksnui_gpu_create(NULL, W, H);
    if (!dev) { printf("FAIL: gpu_create\n"); return 1; }

    if (ksnui_gpu_begin(dev, 0.1f, 0.1f, 0.1f, 1.0f) != 0) {
        printf("FAIL: begin\n"); return 1;
    }

    // Rounded rect: x=20 y=30 w=60 h=40 r=8, color 0.2 0.6 0.9 1.0
    ksnui_Rect rect = { 20.0f, 30.0f, 60.0f, 40.0f, 8.0f, 0.2f, 0.6f, 0.9f, 1.0f };
    ksnui_gpu_draw_rects(dev, &rect, 1);

    ksnui_gpu_end(dev);

    uint8_t* rgba = malloc((size_t)W * H * 4);
    if (!rgba) { printf("FAIL: malloc\n"); return 1; }
    size_t got = ksnui_gpu_readback(dev, rgba, (size_t)W * H * 4);
    if (got != (size_t)W * H * 4) { printf("FAIL: readback got %zu\n", got); return 1; }

    uint32_t ck = checksum(rgba, got);
    printf("checksum=%u\n", ck);

    if (ksnui_png_write("/tmp/opencode/smoke_test_native.png", W, H, rgba) != 0) {
        printf("FAIL: png_write\n"); return 1;
    }

    ksnui_gpu_destroy(dev);
    free(rgba);
    printf("ALL OK\n");
    return 0;
}
