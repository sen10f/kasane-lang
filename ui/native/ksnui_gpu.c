// ksnui_gpu.c: libksnui の GPU (wgpu-native) 実装。ksnui_gpu.h の平坦な API を実現する。
// headless (window == NULL) のみをサポート。ウィンドウありの surface は将来対応。
#include "ksnui_gpu.h"
#include <webgpu/webgpu.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ---------------------------------------------------------------- デバイス状態 */
typedef struct {
    WGPUInstance instance;
    WGPUAdapter adapter;
    WGPUDevice device;
    WGPUQueue queue;
    WGPUTexture offscreen;     /* headless の描画先テクスチャ (RGBA8Unorm) */
    WGPUTexture atlas;         /* グリフアトラス (R8Unorm, 1024x1024) */
    WGPUTextureView atlas_view;
    WGPUSampler sampler;
    WGPUBindGroupLayout bgl;   /* atlas texture + sampler */
    WGPUBindGroup bind_group;  /* atlas + sampler */
    WGPURenderPipeline rect_pipe;    /* 角丸四角用パイプライン */
    WGPURenderPipeline glyph_pipe;   /* グリフ用パイプライン */
    int32_t width, height;
    int headless;
} KsnuiGpu;

static KsnuiGpu g_gpu;
static int g_gpu_ready = 0;

/* 非同期完了を同期風に待つためのフラグとコールバック。 */
static int g_sync_flag = 0;

static int wait_sync(WGPUInstance inst);

static void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter,
                       WGPUStringView message, void* u1, void* u2) {
    (void)status; (void)message; (void)u1; (void)u2;
    g_gpu.adapter = adapter;
    g_sync_flag = 1;
}

static void on_device(WGPURequestDeviceStatus status, WGPUDevice device,
                      WGPUStringView message, void* u1, void* u2) {
    (void)status; (void)message; (void)u1; (void)u2;
    g_gpu.device = device;
    g_sync_flag = 1;
}

static void on_map_done(WGPUMapAsyncStatus status, WGPUStringView message,
                        void* u1, void* u2) {
    (void)status; (void)message; (void)u1; (void)u2;
    g_sync_flag = 1;
}

static void on_work_done(WGPUQueueWorkDoneStatus status, WGPUStringView message,
                         void* u1, void* u2) {
    (void)status; (void)message; (void)u1; (void)u2;
    g_sync_flag = 1;
}

static void on_uncaptured_error(WGPUDevice const *dev, WGPUErrorType type,
                                WGPUStringView msg, void* u1, void* u2) {
    (void)dev; (void)u1; (void)u2;
    printf("[wgpu error] type=%d msg=%.*s\n", (int)type, (int)msg.length,
           msg.data ? (const char*)msg.data : "");
}

/* 頂点データを GPU 上のバッファにアップロードする（ステージング＋コピー）。
   wgpuQueueWriteBuffer はコマンドバッファ提出との順序保証が不確実なため、
   mappedAtCreation なステージングバッファ経由で copyBufferToBuffer し、
   同期バッファへのコピー＋MapRead で完了を待つ。 */
static WGPUBuffer upload_vertices(const float* data, size_t size) {
    WGPUBuffer staging = wgpuDeviceCreateBuffer(g_gpu.device, &(WGPUBufferDescriptor){
        .size = size, .usage = WGPUBufferUsage_MapWrite | WGPUBufferUsage_CopySrc,
        .mappedAtCreation = 1 });
    if (!staging) return NULL;
    void* smap = wgpuBufferGetMappedRange(staging, 0, size);
    if (!smap) { wgpuBufferRelease(staging); return NULL; }
    memcpy(smap, data, size);
    wgpuBufferUnmap(staging);

    WGPUBuffer vbuf = wgpuDeviceCreateBuffer(g_gpu.device, &(WGPUBufferDescriptor){
        .size = size, .usage = WGPUBufferUsage_Vertex | WGPUBufferUsage_CopyDst | WGPUBufferUsage_CopySrc });
    if (!vbuf) { wgpuBufferRelease(staging); return NULL; }

    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_gpu.device, NULL);
    wgpuCommandEncoderCopyBufferToBuffer(enc, staging, 0, vbuf, 0, size);
    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(enc, NULL);
    wgpuQueueSubmit(g_gpu.queue, 1, &cmd);
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(enc);

    /* 完了待ち: vb の先頭 4 バイトを sync バッファへコピーして MapRead する。 */
    WGPUBuffer sync = wgpuDeviceCreateBuffer(g_gpu.device, &(WGPUBufferDescriptor){
        .size = 4, .usage = WGPUBufferUsage_MapRead | WGPUBufferUsage_CopyDst });
    WGPUCommandEncoder enc2 = wgpuDeviceCreateCommandEncoder(g_gpu.device, NULL);
    wgpuCommandEncoderCopyBufferToBuffer(enc2, vbuf, 0, sync, 0, 4);
    WGPUCommandBuffer cmd2 = wgpuCommandEncoderFinish(enc2, NULL);
    wgpuQueueSubmit(g_gpu.queue, 1, &cmd2);
    wgpuCommandBufferRelease(cmd2);
    wgpuCommandEncoderRelease(enc2);

    g_sync_flag = 0;
    wgpuBufferMapAsync(sync, WGPUMapMode_Read, 0, 4,
                       (WGPUBufferMapCallbackInfo){ .mode = WGPUCallbackMode_AllowProcessEvents,
                                                   .callback = on_map_done });
    int ws = wait_sync(g_gpu.instance);
    if (ws < 0) {
        wgpuBufferDestroy(sync); wgpuBufferRelease(sync);
        wgpuBufferRelease(staging); wgpuBufferRelease(vbuf);
        return NULL;
    }
    wgpuBufferUnmap(sync);
    wgpuBufferDestroy(sync);
    wgpuBufferRelease(sync);
    wgpuBufferRelease(staging);

    return vbuf;
}

/* コールバックが立つまで wgpuInstanceProcessEvents を回す (同期風待ち)。
   タイムアウト付き。成功で 0、タイムアウトで -1。 */
static int wait_sync(WGPUInstance inst) {
    /* 呼び出し元が非同期リクエストの直前に g_sync_flag = 0 とする。
       コールバックが同期的に発火しても即座に検出する。 */
    for (int i = 0; i < 200000; i++) {
        wgpuInstanceProcessEvents(inst);
        if (g_sync_flag) return 0;
    }
    return -1;
}

/* ---------------------------------------------------------------- PNG 書き出し */
/* zlib (ストアド deflate ブロック) + PNG。libpng に依存させない最小実装。 */

static void put_be32(uint8_t* p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

/* WGPUStringView は { data, length } の構造体。文字リテラルから作る。 */
static WGPUStringView sv(const char* s) {
    return (WGPUStringView){ .data = s, .length = strlen(s) };
}

static uint32_t crc32_update(uint32_t crc, const uint8_t* data, size_t len) {
    static uint32_t table[256];
    static int table_init = 0;
    if (!table_init) {
        for (uint32_t i = 0; i < 256; i++) {
            uint32_t c = i;
            for (int j = 0; j < 8; j++)
                c = (c & 1) ? (0xEDB88320u ^ (c >> 1)) : (c >> 1);
            table[i] = c;
        }
        table_init = 1;
    }
    for (size_t i = 0; i < len; i++)
        crc = table[(crc ^ data[i]) & 0xFF] ^ (crc >> 8);
    return crc;
}

/* 1 チャンク書き出し (長さ+種別+データ+CRC)。 */
static int png_write_chunk(FILE* f, const char* type, const uint8_t* data,
                           size_t data_len) {
    uint8_t header[8];
    put_be32(header, (uint32_t)data_len);
    memcpy(header + 4, type, 4);
    if (fwrite(header, 1, 8, f) != 8) return 1;
    if (data_len && fwrite(data, 1, data_len, f) != data_len) return 1;
    uint32_t crc = 0xFFFFFFFFu;
    crc = crc32_update(crc, (const uint8_t*)type, 4);
    if (data_len) crc = crc32_update(crc, data, data_len);
    crc ^= 0xFFFFFFFFu;
    uint8_t crcb[4];
    put_be32(crcb, crc);
    return fwrite(crcb, 1, 4, f) != 4;
}

/* deflate ストアドブロックを出力 (BFINAL=最終か, data/len)。
   1 バイトヘッダ + LEN(2, LE) + NLEN(2, LE) + 生データ。 */
static size_t deflate_stored_block(uint8_t* out, int final, const uint8_t* data,
                                   size_t len) {
    size_t p = 0;
    out[p++] = (uint8_t)(final ? 0x01 : 0x00); /* BFINAL + BTYPE=00 (stored) */
    out[p++] = (uint8_t)(len & 0xFF);
    out[p++] = (uint8_t)((len >> 8) & 0xFF);
    out[p++] = (uint8_t)((~len) & 0xFF);
    out[p++] = (uint8_t)((~len >> 8) & 0xFF);
    if (len) { memcpy(out + p, data, len); p += len; }
    return p;
}

int32_t ksnui_png_write(const char* path, int32_t w, int32_t h, const uint8_t* rgba) {
    if (!path || w <= 0 || h <= 0 || !rgba) return 1;
    FILE* f = fopen(path, "wb");
    if (!f) return 1;
    int ret = 1;

    static const uint8_t sig[8] = {0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A};
    if (fwrite(sig, 1, 8, f) != 8) goto end;

    /* IHDR */
    {
        uint8_t ihdr[13];
        put_be32(ihdr, (uint32_t)w);
        put_be32(ihdr + 4, (uint32_t)h);
        ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
        if (png_write_chunk(f, "IHDR", ihdr, 13)) goto end;
    }

    /* IDAT: 各行の先頭にフィルタバイト(0x00=なし)を入れた
       zlib(ストアド deflate ブロック)+adler32 を 1 つの IDAT チャンクにする。 */
    {
        size_t row_stride = (size_t)w * 4;
        size_t filtered_len = ((size_t)h) * (1 + row_stride);
        uint8_t* filtered = (uint8_t*)malloc(filtered_len);
        if (!filtered) goto end;
        for (int32_t y = 0; y < h; y++) {
            uint8_t* row = filtered + (size_t)y * (1 + row_stride);
            row[0] = 0; /* フィルタ: なし */
            memcpy(row + 1, rgba + (size_t)y * row_stride, row_stride);
        }

        uint8_t* idat = NULL;
        size_t idat_cap = 0, idat_len = 0;
        int idat_ok = 1;
        #define IDAT_APPEND(p, n) do {                                         \
            if (idat_len + (n) > idat_cap) {                                   \
                size_t nc = idat_cap ? idat_cap * 2 : 4096;                    \
                while (nc < idat_len + (n)) nc *= 2;                           \
                uint8_t* np = (uint8_t*)realloc(idat, nc);                     \
                if (!np) { idat_ok = 0; break; }                               \
                idat = np; idat_cap = nc;                                      \
            }                                                                  \
            memcpy(idat + idat_len, (p), (n)); idat_len += (n);                \
        } while (0)

        uint32_t adler = 1;
        uint8_t zlib_header[2] = {0x78, 0x01};
        IDAT_APPEND(zlib_header, 2);

        size_t off = 0;
        while (idat_ok && off < filtered_len) {
            size_t chunk = filtered_len - off;
            if (chunk > 65535) chunk = 65535;
            int final = (off + chunk >= filtered_len) ? 1 : 0;
            uint8_t block[65535 + 5];
            size_t blen = deflate_stored_block(block, final ? 1 : 0,
                                               filtered + off, chunk);
            IDAT_APPEND(block, blen);
            for (size_t i = 0; i < chunk; i++) adler = (adler + filtered[off + i]) % 65521u;
            off += chunk;
        }
        if (idat_ok) {
            uint8_t adler_buf[4];
            put_be32(adler_buf, adler);
            IDAT_APPEND(adler_buf, 4);
        }
        #undef IDAT_APPEND

        free(filtered);

        if (!idat_ok || !idat || png_write_chunk(f, "IDAT", idat, idat_len))
            goto end;
        free(idat);
    }

    /* IEND */
    if (png_write_chunk(f, "IEND", NULL, 0)) goto end;

    ret = 0;
end:
    fclose(f);
    return ret;
}

/* ---------------------------------------------------------------- シェーダー (WGSL) */
/* 論理 px は C 側で NDC (-1..1) に変換して頂点バッファに入れる。 */
static const char* vs_rect_wgsl =
"struct VertexOut {\n"
"  @builtin(position) position : vec4<f32>,\n"
"  @location(0) rmin_x : f32,\n"
"  @location(1) rmin_y : f32,\n"
"  @location(2) rmax_x : f32,\n"
"  @location(3) rmax_y : f32,\n"
"  @location(4) radius : f32,\n"
"  @location(5) cr : f32,\n"
"  @location(6) cg : f32,\n"
"  @location(7) cb : f32,\n"
"  @location(8) ca : f32,\n"
"}\n"
"@vertex\n"
"fn vs_main(@location(0) x : f32, @location(1) y : f32,\n"
"           @location(2) rmin_x : f32, @location(3) rmin_y : f32,\n"
"           @location(4) rmax_x : f32, @location(5) rmax_y : f32,\n"
"           @location(6) r : f32,\n"
"           @location(7) cr : f32, @location(8) cg : f32,\n"
"           @location(9) cb : f32, @location(10) ca : f32) -> VertexOut {\n"
"  var out : VertexOut;\n"
"  out.position = vec4<f32>(x, y, 0.0, 1.0);\n"
"  out.rmin_x = rmin_x; out.rmin_y = rmin_y;\n"
"  out.rmax_x = rmax_x; out.rmax_y = rmax_y;\n"
"  out.radius = r;\n"
"  out.cr = cr; out.cg = cg; out.cb = cb; out.ca = ca;\n"
"  return out;\n"
"}\n";

static const char* fs_rect_wgsl =
"@fragment\n"
"fn fs_main(@builtin(position) frag_pos : vec4<f32>,\n"
"           @location(0) rmin_x : f32, @location(1) rmin_y : f32,\n"
"           @location(2) rmax_x : f32, @location(3) rmax_y : f32,\n"
"           @location(4) radius : f32,\n"
"           @location(5) cr : f32, @location(6) cg : f32,\n"
"           @location(7) cb : f32, @location(8) ca : f32) -> @location(0) vec4<f32> {\n"
"  let p = frag_pos.xy;\n"
"  let rect_min = vec2<f32>(rmin_x, rmin_y);\n"
"  let rect_max = vec2<f32>(rmax_x, rmax_y);\n"
"  let color = vec4<f32>(cr, cg, cb, ca);\n"
"  if (p.x < rect_min.x || p.x > rect_max.x || p.y < rect_min.y || p.y > rect_max.y) {\n"
"    discard;\n"
"  }\n"
"  if (radius > 0.0) {\n"
"    if (p.x < rect_min.x + radius && p.y < rect_min.y + radius) {\n"
"      if (distance(p, rect_min + vec2<f32>(radius, radius)) > radius) { discard; }\n"
"    }\n"
"    if (p.x > rect_max.x - radius && p.y < rect_min.y + radius) {\n"
"      if (distance(p, vec2<f32>(rect_max.x - radius, rect_min.y + radius)) > radius) { discard; }\n"
"    }\n"
"    if (p.x < rect_min.x + radius && p.y > rect_max.y - radius) {\n"
"      if (distance(p, vec2<f32>(rect_min.x + radius, rect_max.y - radius)) > radius) { discard; }\n"
"    }\n"
"    if (p.x > rect_max.x - radius && p.y > rect_max.y - radius) {\n"
"      if (distance(p, rect_max - vec2<f32>(radius, radius)) > radius) { discard; }\n"
"    }\n"
"  }\n"
"  return color;\n"
"}\n";

static const char* vs_glyph_wgsl =
"struct VertexOut {\n"
"  @builtin(position) position : vec4<f32>,\n"
"  @location(0) uv : vec2<f32>,\n"
"  @location(1) color : vec4<f32>,\n"
"}\n"
"@vertex\n"
"fn vs_main(@location(0) x : f32, @location(1) y : f32,\n"
"           @location(2) u0 : f32, @location(3) v0 : f32,\n"
"           @location(4) u1 : f32, @location(5) v1 : f32,\n"
"           @location(6) cr : f32, @location(7) cg : f32,\n"
"           @location(8) cb : f32, @location(9) ca : f32) -> VertexOut {\n"
"  var out : VertexOut;\n"
"  out.position = vec4<f32>(x, y, 0.0, 1.0);\n"
"  out.uv = vec2<f32>(u0, v0);\n"
"  out.color = vec4<f32>(cr, cg, cb, ca);\n"
"  return out;\n"
"}\n";

static const char* fs_glyph_wgsl =
"@group(0) @binding(0) var atlas_texture : texture_2d<f32>;\n"
"@group(0) @binding(1) var atlas_sampler : sampler;\n"
"@fragment\n"
"fn fs_main(@builtin(position) frag_pos : vec4<f32>,\n"
"           @location(0) uv : vec2<f32>,\n"
"           @location(1) color : vec4<f32>) -> @location(0) vec4<f32> {\n"
"  let a = textureSample(atlas_texture, atlas_sampler, uv).a;\n"
"  return vec4<f32>(color.rgb, color.a * a);\n"
"}\n";

/* ---------------------------------------------------------------- ユーティリティ */
static WGPUShaderModule make_shader(WGPUDevice dev, const char* wgsl) {
    WGPUShaderSourceWGSL wgsl_src = {0};
    wgsl_src.chain.next = NULL;
    wgsl_src.chain.sType = WGPUSType_ShaderSourceWGSL;
    wgsl_src.code = (WGPUStringView){ .data = wgsl, .length = strlen(wgsl) };
    WGPUShaderModuleDescriptor desc = {0};
    desc.nextInChain = &wgsl_src.chain;
    return wgpuDeviceCreateShaderModule(dev, &desc);
}

static WGPURenderPipeline make_rect_pipeline(WGPUDevice dev) {
    WGPUShaderModule vs_mod = make_shader(dev, vs_rect_wgsl);
    WGPUShaderModule fs_mod = make_shader(dev, fs_rect_wgsl);

    WGPUVertexAttribute attrs[11] = {0};
    for (int i = 0; i < 11; i++) {
        attrs[i].format = WGPUVertexFormat_Float32;
        attrs[i].offset = (uint64_t)(i * 4);
        attrs[i].shaderLocation = (uint32_t)i;
    }
    WGPUVertexBufferLayout layout = {0};
    layout.arrayStride = 11 * 4;
    layout.stepMode = WGPUVertexStepMode_Vertex;
    layout.attributeCount = 11;
    layout.attributes = attrs;

    WGPUVertexState vs = {0};
    vs.module = vs_mod;
    vs.entryPoint = sv("vs_main");
    vs.bufferCount = 1;
    vs.buffers = &layout;

    WGPUColorTargetState target = {0};
    target.format = WGPUTextureFormat_RGBA8Unorm;
    WGPUBlendState blend_state = {
        .color = { .srcFactor = WGPUBlendFactor_SrcAlpha,
                   .dstFactor = WGPUBlendFactor_OneMinusSrcAlpha,
                   .operation = WGPUBlendOperation_Add },
        .alpha = { .srcFactor = WGPUBlendFactor_One,
                   .dstFactor = WGPUBlendFactor_OneMinusSrcAlpha,
                   .operation = WGPUBlendOperation_Add },
    };
    target.blend = &blend_state;
    target.writeMask = WGPUColorWriteMask_All;

    WGPUFragmentState fs = {0};
    fs.module = fs_mod;
    fs.entryPoint = sv("fs_main");
    fs.targetCount = 1;
    fs.targets = &target;

    WGPUPrimitiveState prim = {0};
    prim.topology = WGPUPrimitiveTopology_TriangleStrip;

    WGPUMultisampleState ms = { .count = 1, .mask = 0xFFFFFFFF };

    WGPURenderPipelineDescriptor desc = {0};
    desc.vertex = vs;
    desc.fragment = &fs;
    desc.primitive = prim;
    desc.multisample = ms;
    desc.layout = NULL;

    WGPURenderPipeline pipe = wgpuDeviceCreateRenderPipeline(dev, &desc);
    wgpuShaderModuleRelease(vs_mod);
    wgpuShaderModuleRelease(fs_mod);
    return pipe;
}

static WGPURenderPipeline make_glyph_pipeline(WGPUDevice dev, WGPUBindGroupLayout bgl) {
    WGPUShaderModule vs_mod = make_shader(dev, vs_glyph_wgsl);
    WGPUShaderModule fs_mod = make_shader(dev, fs_glyph_wgsl);

    WGPUVertexAttribute attrs[10] = {0};
    for (int i = 0; i < 10; i++) {
        attrs[i].format = WGPUVertexFormat_Float32;
        attrs[i].offset = (uint64_t)(i * 4);
        attrs[i].shaderLocation = (uint32_t)i;
    }
    WGPUVertexBufferLayout layout = {0};
    layout.arrayStride = 10 * 4;
    layout.stepMode = WGPUVertexStepMode_Vertex;
    layout.attributeCount = 10;
    layout.attributes = attrs;

    WGPUVertexState vs = {0};
    vs.module = vs_mod;
    vs.entryPoint = sv("vs_main");
    vs.bufferCount = 1;
    vs.buffers = &layout;

    WGPUColorTargetState target = {0};
    target.format = WGPUTextureFormat_RGBA8Unorm;
    WGPUBlendState blend_state = {
        .color = { .srcFactor = WGPUBlendFactor_SrcAlpha,
                   .dstFactor = WGPUBlendFactor_OneMinusSrcAlpha,
                   .operation = WGPUBlendOperation_Add },
        .alpha = { .srcFactor = WGPUBlendFactor_One,
                   .dstFactor = WGPUBlendFactor_OneMinusSrcAlpha,
                   .operation = WGPUBlendOperation_Add },
    };
    target.blend = &blend_state;
    target.writeMask = WGPUColorWriteMask_All;

    WGPUFragmentState fs = {0};
    fs.module = fs_mod;
    fs.entryPoint = sv("fs_main");
    fs.targetCount = 1;
    fs.targets = &target;

    WGPUPrimitiveState prim = {0};
    prim.topology = WGPUPrimitiveTopology_TriangleStrip;

    WGPUMultisampleState ms = { .count = 1, .mask = 0xFFFFFFFF };

    WGPUPipelineLayoutDescriptor pl_desc = {0};
    pl_desc.bindGroupLayoutCount = 1;
    pl_desc.bindGroupLayouts = &bgl;

    WGPURenderPipelineDescriptor desc = {0};
    desc.vertex = vs;
    desc.fragment = &fs;
    desc.primitive = prim;
    desc.multisample = ms;
    desc.layout = wgpuDeviceCreatePipelineLayout(dev, &pl_desc);

    WGPURenderPipeline pipe = wgpuDeviceCreateRenderPipeline(dev, &desc);
    wgpuShaderModuleRelease(vs_mod);
    wgpuShaderModuleRelease(fs_mod);
    wgpuPipelineLayoutRelease(desc.layout);
    return pipe;
}

/* 論理 px (左上原点, Y 下方向) → NDC (-1..1, Y 上方向)。 */
static void to_ndc(int32_t w, int32_t h, float x, float y, float sx, float sy,
                   float* out_x, float* out_y) {
    *out_x = (2.0f * x / (float)w) - 1.0f;
    *out_x += (2.0f * sx / (float)w);
    *out_y = 1.0f - (2.0f * y / (float)h);
    *out_y -= (2.0f * sy / (float)h);
}

/* ---------------------------------------------------------------- GPU 作成 */
void* ksnui_gpu_create(void* window, int32_t w, int32_t h) {
    (void)window; /* ウィンドウあり surface は未対応。常に headless。 */
    memset(&g_gpu, 0, sizeof(g_gpu));
    g_gpu.width = w;
    g_gpu.height = h;
    g_gpu.headless = 1;

    g_gpu.instance = wgpuCreateInstance(NULL);
    if (!g_gpu.instance) return NULL;

    /* Adapter (非同期を同期風に) */
    WGPURequestAdapterCallbackInfo acb = {0};
    acb.mode = 0;
    acb.callback = on_adapter;
    g_sync_flag = 0;
    wgpuInstanceRequestAdapter(g_gpu.instance, NULL, acb);
    if (wait_sync(g_gpu.instance) < 0 || !g_gpu.adapter) return NULL;

    /* Device (非同期を同期風に) */
    WGPURequestDeviceCallbackInfo dcb = {0};
    dcb.mode = 0;
    dcb.callback = on_device;
    g_sync_flag = 0;
    /* デバイス作成時に未キャプチャエラーのコールバックを設定し、
       パイプライン作成失敗などを可視化する。 */
    WGPUDeviceDescriptor ddesc = {0};
    ddesc.uncapturedErrorCallbackInfo = (WGPUUncapturedErrorCallbackInfo){
        .callback = on_uncaptured_error,
    };
    wgpuAdapterRequestDevice(g_gpu.adapter, &ddesc, dcb);
    if (wait_sync(g_gpu.instance) < 0 || !g_gpu.device) return NULL;

    g_gpu.queue = wgpuDeviceGetQueue(g_gpu.device);

    /* Headless: オフスクリーンテクスチャ (RGBA8Unorm) */
    WGPUTextureDescriptor tex_desc = {0};
    tex_desc.size.width = (uint32_t)w;
    tex_desc.size.height = (uint32_t)h;
    tex_desc.size.depthOrArrayLayers = 1;
    tex_desc.mipLevelCount = 1;
    tex_desc.sampleCount = 1;
    tex_desc.dimension = WGPUTextureDimension_2D;
    tex_desc.format = WGPUTextureFormat_RGBA8Unorm;
    tex_desc.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    g_gpu.offscreen = wgpuDeviceCreateTexture(g_gpu.device, &tex_desc);

    /* Atlas テクスチャ (1024x1024 R8Unorm) */
    WGPUTextureDescriptor atlas_desc = {0};
    atlas_desc.size.width = 1024;
    atlas_desc.size.height = 1024;
    atlas_desc.size.depthOrArrayLayers = 1;
    atlas_desc.mipLevelCount = 1;
    atlas_desc.sampleCount = 1;
    atlas_desc.dimension = WGPUTextureDimension_2D;
    atlas_desc.format = WGPUTextureFormat_R8Unorm;
    atlas_desc.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
    g_gpu.atlas = wgpuDeviceCreateTexture(g_gpu.device, &atlas_desc);

    WGPUTextureViewDescriptor view_desc = {0};
    view_desc.format = WGPUTextureFormat_R8Unorm;
    view_desc.dimension = WGPUTextureViewDimension_2D;
    view_desc.baseMipLevel = 0;
    view_desc.mipLevelCount = 1;
    view_desc.baseArrayLayer = 0;
    view_desc.arrayLayerCount = 1;
    g_gpu.atlas_view = wgpuTextureCreateView(g_gpu.atlas, &view_desc);

    /* サンプラー */
    WGPUSamplerDescriptor samp_desc = {0};
    samp_desc.magFilter = WGPUFilterMode_Linear;
    samp_desc.minFilter = WGPUFilterMode_Linear;
    samp_desc.mipmapFilter = WGPUMipmapFilterMode_Linear;
    samp_desc.maxAnisotropy = 1;
    samp_desc.addressModeU = WGPUAddressMode_ClampToEdge;
    samp_desc.addressModeV = WGPUAddressMode_ClampToEdge;
    samp_desc.addressModeW = WGPUAddressMode_ClampToEdge;
    g_gpu.sampler = wgpuDeviceCreateSampler(g_gpu.device, &samp_desc);

    /* Bind group layout (atlas texture + sampler) */
    WGPUBindGroupLayoutEntry entries[2] = {0};
    entries[0].binding = 0;
    entries[0].visibility = WGPUShaderStage_Fragment;
    entries[0].texture = (WGPUTextureBindingLayout){
        .sampleType = WGPUTextureSampleType_Float,
        .viewDimension = WGPUTextureViewDimension_2D,
    };
    entries[1].binding = 1;
    entries[1].visibility = WGPUShaderStage_Fragment;
    entries[1].sampler = (WGPUSamplerBindingLayout){
        .type = WGPUSamplerBindingType_Filtering,
    };
    WGPUBindGroupLayoutDescriptor bgl_desc = {0};
    bgl_desc.entryCount = 2;
    bgl_desc.entries = entries;
    g_gpu.bgl = wgpuDeviceCreateBindGroupLayout(g_gpu.device, &bgl_desc);

    /* Bind group */
    WGPUBindGroupEntry bgentries[2] = {0};
    bgentries[0].binding = 0;
    bgentries[0].textureView = g_gpu.atlas_view;
    bgentries[1].binding = 1;
    bgentries[1].sampler = g_gpu.sampler;
    WGPUBindGroupDescriptor bgdesc = {0};
    bgdesc.entryCount = 2;
    bgdesc.entries = bgentries;
    bgdesc.layout = g_gpu.bgl;
    g_gpu.bind_group = wgpuDeviceCreateBindGroup(g_gpu.device, &bgdesc);

    /* パイプライン */
    g_gpu.rect_pipe = make_rect_pipeline(g_gpu.device);
    g_gpu.glyph_pipe = make_glyph_pipeline(g_gpu.device, g_gpu.bgl);

    g_gpu_ready = 1;
    return &g_gpu;
}

void ksnui_gpu_destroy(void* dev) {
    (void)dev;
    if (!g_gpu_ready) return;
    if (g_gpu.bind_group) wgpuBindGroupRelease(g_gpu.bind_group);
    if (g_gpu.rect_pipe) wgpuRenderPipelineRelease(g_gpu.rect_pipe);
    if (g_gpu.glyph_pipe) wgpuRenderPipelineRelease(g_gpu.glyph_pipe);
    if (g_gpu.bgl) wgpuBindGroupLayoutRelease(g_gpu.bgl);
    if (g_gpu.atlas_view) wgpuTextureViewRelease(g_gpu.atlas_view);
    if (g_gpu.atlas) wgpuTextureRelease(g_gpu.atlas);
    if (g_gpu.offscreen) wgpuTextureRelease(g_gpu.offscreen);
    if (g_gpu.sampler) wgpuSamplerRelease(g_gpu.sampler);
    if (g_gpu.queue) wgpuQueueRelease(g_gpu.queue);
    if (g_gpu.device) wgpuDeviceRelease(g_gpu.device);
    if (g_gpu.adapter) wgpuAdapterRelease(g_gpu.adapter);
    if (g_gpu.instance) wgpuInstanceRelease(g_gpu.instance);
    memset(&g_gpu, 0, sizeof(g_gpu));
    g_gpu_ready = 0;
}

int32_t ksnui_gpu_resize(void* dev, int32_t w, int32_t h) {
    (void)dev;
    if (!g_gpu_ready) return 1;
    if (g_gpu.offscreen) wgpuTextureRelease(g_gpu.offscreen);
    WGPUTextureDescriptor tex_desc = {0};
    tex_desc.size.width = (uint32_t)w;
    tex_desc.size.height = (uint32_t)h;
    tex_desc.size.depthOrArrayLayers = 1;
    tex_desc.mipLevelCount = 1;
    tex_desc.sampleCount = 1;
    tex_desc.dimension = WGPUTextureDimension_2D;
    tex_desc.format = WGPUTextureFormat_RGBA8Unorm;
    tex_desc.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    g_gpu.offscreen = wgpuDeviceCreateTexture(g_gpu.device, &tex_desc);
    g_gpu.width = w;
    g_gpu.height = h;
    return 0;
}

int32_t ksnui_gpu_begin(void* dev, float r, float g, float b, float a) {
    (void)dev;
    (void)r;
    (void)g;
    (void)b;
    (void)a;
    if (!g_gpu_ready) return 1;
    return 0;
}

static WGPUTextureView offscreen_view(void) {
    return wgpuTextureCreateView(g_gpu.offscreen, &(WGPUTextureViewDescriptor){
        .format = WGPUTextureFormat_RGBA8Unorm,
        .dimension = WGPUTextureViewDimension_2D,
        .baseMipLevel = 0, .mipLevelCount = 1,
        .baseArrayLayer = 0, .arrayLayerCount = 1,
    });
}

void ksnui_gpu_draw_rects(void* dev, const ksnui_Rect* rects, size_t n) {
    (void)dev;
    if (!g_gpu_ready || !rects || n == 0) return;
    /* 1 四角 = 4 頂点 (TriangleStrip)、11 float/頂点:
       (x, y) 位置、(rect_min_x, rect_min_y, rect_max_x, rect_max_y) 範囲、
       radius, RGBA 色。シェーダーが rect_min/rect_max で範囲判定し、
       radius で角丸を抜く。 */
    size_t vbuf_size = n * 4 * 11 * 4;
    float* vb = (float*)malloc(vbuf_size);
    if (!vb) return;
    size_t vi = 0;
    for (size_t i = 0; i < n; i++) {
        const ksnui_Rect* rc = &rects[i];
        float x0, y0;
        to_ndc(g_gpu.width, g_gpu.height, rc->x, rc->y, 0.0f, 0.0f, &x0, &y0);
        /* NDC 幅・高さ (Y 上方向なので下方向へはマイナス) */
        float nx = (2.0f * rc->w / (float)g_gpu.width);
        float ny = (2.0f * rc->h / (float)g_gpu.height);
        /* フラグメントシェーダーは frag_pos.xy (ピクセル空間) と rect 範囲を比較するため、
           rect 範囲はピクセル空間で渡す。radius もピクセル単位。 */
        float rmin_x = (float)rc->x, rmin_y = (float)rc->y;
        float rmax_x = (float)(rc->x + rc->w), rmax_y = (float)(rc->y + rc->h);
        float r_px = (float)rc->r;
        /* TriangleStrip: 左上, 左下, 右上, 右下。全頂点で同じ rect_min/max。 */
        float v[4][11] = {
            { x0,     y0 - ny, rmin_x, rmin_y, rmax_x, rmax_y, r_px,
              rc->cr, rc->cg, rc->cb, rc->ca },
            { x0,     y0,      rmin_x, rmin_y, rmax_x, rmax_y, r_px,
              rc->cr, rc->cg, rc->cb, rc->ca },
            { x0 + nx, y0 - ny, rmin_x, rmin_y, rmax_x, rmax_y, r_px,
              rc->cr, rc->cg, rc->cb, rc->ca },
            { x0 + nx, y0,      rmin_x, rmin_y, rmax_x, rmax_y, r_px,
              rc->cr, rc->cg, rc->cb, rc->ca },
        };
        for (int k = 0; k < 4; k++)
            for (int j = 0; j < 11; j++)
                vb[vi++] = v[k][j];
    }

    WGPUBuffer vb_handle = upload_vertices(vb, vbuf_size);
    free(vb);

    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(g_gpu.device, NULL);

    WGPURenderPassColorAttachment att = {0};
    att.view = offscreen_view();
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.loadOp = WGPULoadOp_Clear;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = (WGPUColor){ .r = 0.1, .g = 0.1, .b = 0.1, .a = 1.0 };

    WGPURenderPassDescriptor rp_desc = {0};
    rp_desc.colorAttachmentCount = 1;
    rp_desc.colorAttachments = &att;

    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(encoder, &rp_desc);
    wgpuRenderPassEncoderSetPipeline(pass, g_gpu.rect_pipe);
    wgpuRenderPassEncoderSetViewport(pass, 0.0, 0.0, (float)g_gpu.width, (float)g_gpu.height, 0.0, 1.0);
    wgpuRenderPassEncoderSetScissorRect(pass, 0, 0, (uint32_t)g_gpu.width, (uint32_t)g_gpu.height);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, vb_handle, 0, vbuf_size);
    wgpuRenderPassEncoderDraw(pass, (uint32_t)(n * 4), 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);

    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(encoder, NULL);
    wgpuQueueSubmit(g_gpu.queue, 1, &cmd);
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(encoder);
    wgpuBufferRelease(vb_handle);
    wgpuTextureViewRelease(att.view);
}

void ksnui_gpu_atlas_upload(void* dev, int32_t w, int32_t h, const uint8_t* pixels) {
    (void)dev;
    if (!g_gpu_ready || !pixels || w <= 0 || h <= 0) return;
    WGPUTexelCopyTextureInfo dst = {0};
    dst.texture = g_gpu.atlas;
    WGPUTexelCopyBufferLayout layout = {0};
    layout.offset = 0;
    layout.bytesPerRow = (uint32_t)w;
    layout.rowsPerImage = (uint32_t)h;
    WGPUExtent3D size = {0};
    size.width = (uint32_t)w;
    size.height = (uint32_t)h;
    size.depthOrArrayLayers = 1;
    wgpuQueueWriteTexture(g_gpu.queue, &dst, pixels, (size_t)w * (size_t)h,
                          &layout, &size);
}

void ksnui_gpu_draw_glyphs(void* dev, const ksnui_GlyphQuad* quads, size_t n) {
    (void)dev;
    if (!g_gpu_ready || !quads || n == 0) return;
    size_t vbuf_size = n * 4 * 10 * 4;
    float* vb = (float*)malloc(vbuf_size);
    if (!vb) return;
    size_t vi = 0;
    for (size_t i = 0; i < n; i++) {
        const ksnui_GlyphQuad* q = &quads[i];
        float x0, y0;
        to_ndc(g_gpu.width, g_gpu.height, q->x, q->y, q->w, q->h, &x0, &y0);
        float nx = (2.0f * q->w / (float)g_gpu.width);
        float ny = (2.0f * q->h / (float)g_gpu.height);
        /* TriangleStrip: 左上, 左下, 右上, 右下。UV は atlas (Y 下方向)。u1/v1 は四角共通。 */
        float vs[4][10] = {
            { x0,     y0,     q->u0, q->v0, q->u1, q->v1, q->cr, q->cg, q->cb, q->ca },
            { x0,     y0 - ny, q->u0, q->v1, q->u1, q->v1, q->cr, q->cg, q->cb, q->ca },
            { x0 + nx, y0,     q->u1, q->v0, q->u1, q->v1, q->cr, q->cg, q->cb, q->ca },
            { x0 + nx, y0 - ny, q->u1, q->v1, q->u1, q->v1, q->cr, q->cg, q->cb, q->ca },
        };
        for (int k = 0; k < 4; k++)
            for (int j = 0; j < 10; j++)
                vb[vi++] = vs[k][j];
    }

    WGPUBuffer vb_handle = upload_vertices(vb, vbuf_size);
    free(vb);

    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(g_gpu.device, NULL);

    WGPURenderPassColorAttachment att = {0};
    att.view = offscreen_view();
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.loadOp = WGPULoadOp_Load;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = (WGPUColor){ 0 };

    WGPURenderPassDescriptor rp_desc = {0};
    rp_desc.colorAttachmentCount = 1;
    rp_desc.colorAttachments = &att;

    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(encoder, &rp_desc);
    wgpuRenderPassEncoderSetPipeline(pass, g_gpu.glyph_pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, g_gpu.bind_group, 0, NULL);
    wgpuRenderPassEncoderSetViewport(pass, 0.0, 0.0, (float)g_gpu.width, (float)g_gpu.height, 0.0, 1.0);
    wgpuRenderPassEncoderSetScissorRect(pass, 0, 0, (uint32_t)g_gpu.width, (uint32_t)g_gpu.height);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, vb_handle, 0, vbuf_size);
    wgpuRenderPassEncoderDraw(pass, (uint32_t)(n * 4), 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);

    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(encoder, NULL);
    wgpuQueueSubmit(g_gpu.queue, 1, &cmd);
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(encoder);
    wgpuBufferRelease(vb_handle);
    wgpuTextureViewRelease(att.view);
}

void ksnui_gpu_end(void* dev) {
    (void)dev;
    if (!g_gpu_ready) return;
    if (!g_gpu.headless && g_gpu.queue) {
        /* ウィンドウありの場合は present。headless では何もしない。 */
    }
}

size_t ksnui_gpu_readback(void* dev, uint8_t* out, size_t cap) {
    (void)dev;
    if (!g_gpu_ready || !out) return 0;
    size_t width = (size_t)g_gpu.width;
    size_t height = (size_t)g_gpu.height;
    size_t need = width * height * 4;
    if (cap < need) return 0;

    /* wgpu のコピーは bytesPerRow を 256 バイトアライメントにする必要がある。 */
    size_t row_pitch = (width * 4 + 255) & ~(size_t)255;
    size_t rb_size = row_pitch * height;

    /* 読み戻しバッファ (CopyDst + MapRead)。 */
    WGPUBufferDescriptor rb_desc = {0};
    rb_desc.size = rb_size;
    rb_desc.usage = WGPUBufferUsage_CopyDst | WGPUBufferUsage_MapRead;
    rb_desc.mappedAtCreation = 0;
    WGPUBuffer rb = wgpuDeviceCreateBuffer(g_gpu.device, &rb_desc);
    if (!rb) return 0;

    /* テクスチャ → バッファ コピーを提出。 */
    WGPUCommandEncoder encoder = wgpuDeviceCreateCommandEncoder(g_gpu.device, NULL);
    wgpuCommandEncoderCopyTextureToBuffer(
        encoder,
        &(WGPUTexelCopyTextureInfo){ .texture = g_gpu.offscreen },
        &(WGPUTexelCopyBufferInfo){
            .layout = (WGPUTexelCopyBufferLayout){
                .offset = 0,
                .bytesPerRow = (uint32_t)row_pitch,
                .rowsPerImage = (uint32_t)height,
            },
            .buffer = rb,
        },
        &(WGPUExtent3D){
            .width = (uint32_t)width,
            .height = (uint32_t)height,
            .depthOrArrayLayers = 1,
        });
    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(encoder, NULL);
    wgpuQueueSubmit(g_gpu.queue, 1, &cmd);
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(encoder);

    /* マップ完了 (テクスチャ→バッファ コピー後のデータ利用可能) を待つ。 */
    g_sync_flag = 0;
    wgpuBufferMapAsync(rb, WGPUMapMode_Read, 0, rb_size,
                       (WGPUBufferMapCallbackInfo){
                           .mode = 0,
                           .callback = on_map_done,
                       });
    if (wait_sync(g_gpu.instance) < 0) {
        wgpuBufferDestroy(rb);
        wgpuBufferRelease(rb);
        return 0;
    }

    void* mapped = wgpuBufferGetMappedRange(rb, 0, rb_size);
    if (mapped) {
        /* row_pitch でパディングされた行を width*4 に詰めて out へ。 */
        uint8_t* dst = out;
        uint8_t* src = (uint8_t*)mapped;
        for (size_t y = 0; y < height; y++) {
            memcpy(dst, src, width * 4);
            dst += width * 4;
            src += row_pitch;
        }
        wgpuBufferUnmap(rb);
    }
    wgpuBufferDestroy(rb);
    wgpuBufferRelease(rb);
    return mapped ? need : 0;
}
